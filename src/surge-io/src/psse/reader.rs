#![allow(clippy::needless_range_loop)]
// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! PSS/E RAW (.raw) file parser.
//!
//! Parses the PSS/E RAW power flow data format, versions 29–36.
//! Version is read from the case header and gates version-specific parsing
//! (v35+ column-header annotations, v36 extra sections).
//!
//! # File Structure
//! - 3-line header (case ID, base MVA, version, two heading lines)
//! - Data sections (Bus, Load, Fixed Shunt, Generator, Branch, Transformer, ...)
//! - Each section terminated by `0` record (or `Q` for end-of-file)
//!
//! # Supported Sections
//! - Bus Data
//! - Load Data (accumulated into bus Pd/Qd)
//! - Fixed Shunt Data (accumulated into bus Gs/Bs)
//! - Generator Data
//! - Non-Transformer Branch Data
//! - Transformer Data (2-winding and 3-winding)
//! - Area Interchange Data (stored as metadata in `network.area_schedules`)
//! - Two-terminal HVDC link data (normalized into `network.hvdc.links`)
//! - VSC HVDC link data (normalized into `network.hvdc.links`)
//! - FACTS Device Data (stored in `network.facts_devices`; expanded before NR solve)
//! - Switched Shunt Data (BINIT treated as fixed susceptance; no step control)
//!
//! - Impedance Correction Data (stored in `network.metadata.impedance_corrections`)
//! - Multi-terminal DC line data (normalized into `network.hvdc.dc_grids`)
//! - Multi-Section Line Data (stored in `network.metadata.multi_section_line_groups`)
//! - Zone Data (stored in `network.metadata.regions`)
//! - Inter-Area Transfer Data (stored in `network.metadata.scheduled_area_transfers`)
//! - Owner Data (stored in `network.metadata.owners`)

use std::collections::HashMap;
use std::path::Path;

use surge_network::Network;
use surge_network::network::AreaSchedule;
use surge_network::network::Owner;
use surge_network::network::Region;
use surge_network::network::facts::FactsType;
use surge_network::network::impedance_correction::ImpedanceCorrectionTable;
use surge_network::network::multi_section_line::MultiSectionLineGroup;
use surge_network::network::scheduled_area_transfer::ScheduledAreaTransfer;
use surge_network::network::topology::{
    BusbarSection, ConnectivityNode, Substation as SubstationData, TerminalConnection,
    TopologyMapping, VoltageLevel,
};
use surge_network::network::{Branch, BranchOpfControl, BranchType, Bus, BusType, Generator};
use surge_network::network::{
    DcBranch, DcBus, DcConverter, LccConverterTerminal, LccDcConverter, LccDcConverterRole,
    LccHvdcControlMode, LccHvdcLink,
};
use surge_network::network::{FactsDevice, FactsMode};
use surge_network::network::{NodeBreakerTopology, SwitchDevice, SwitchType};
use surge_network::network::{OltcSpec, ParSpec};
use surge_network::network::{
    VscConverterAcControlMode, VscConverterTerminal, VscHvdcControlMode, VscHvdcLink,
};
use thiserror::Error;

use super::multi_terminal_dc::{RawMtdcBus, RawMtdcConverter, RawMtdcLink, RawMtdcSystem};

#[derive(Error, Debug)]
pub enum PsseError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error on line {line}: {message}")]
    Parse { line: usize, message: String },

    #[error("missing section: {0}")]
    MissingSection(String),

    #[error("unsupported PSS/E version: {0}")]
    UnsupportedVersion(u32),

    #[error("unexpected end of file in {0} section")]
    UnexpectedEof(String),

    /// PE-04: parse_f64 returned a non-finite (NaN or Inf) value.
    #[error("non-finite float value on line {line}: {message}")]
    NonFiniteValue { line: usize, message: String },
}

/// Parse a PSS/E RAW file from disk.
pub fn parse_file(path: &Path) -> Result<Network, PsseError> {
    let content = std::fs::read_to_string(path)?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    parse_string_with_name(&content, &name)
}

/// Parse a PSS/E RAW case from a string.
pub fn parse_str(content: &str) -> Result<Network, PsseError> {
    parse_string_with_name(content, "unknown")
}

fn parse_string_with_name(content: &str, name: &str) -> Result<Network, PsseError> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() < 3 {
        return Err(PsseError::Parse {
            line: 1,
            message: "PSS/E RAW file must have at least 3 header lines".into(),
        });
    }

    // PSS/E v35+ files start with an @!IC,SBASE,REV,... column-header annotation on
    // line 0; the actual case record (IC, SBASE, REV, ...) is on line 1.  Detect this
    // and shift the header index by 1.
    let header_line_idx = if lines[0].starts_with("@!") { 1 } else { 0 };

    // Parse header record 1
    let (sbase, version, freq_hz) = parse_header(lines[header_line_idx], header_line_idx + 1)?;

    let mut network = Network::new(name);
    network.base_mva = sbase;
    network.freq_hz = freq_hz;

    // Start parsing after the 3-line header (shifted by 1 for v35+ @! prefix line).
    let mut pos = header_line_idx + 3;

    // Skip preamble before bus data.  PSS/E v33+ files may include:
    //   - A "SYSTEM-WIDE DATA" section terminated by "0 / END OF SYSTEM-WIDE DATA, BEGIN BUS DATA"
    //   - Solver settings records with non-numeric first tokens (GENERAL, GAUSS, NEWTON, ADJUST …)
    //   - Blank lines and @! column-header annotations
    // All valid bus records begin with a numeric bus number — stop at the first such line.
    while pos < lines.len() {
        let line = lines[pos].trim();
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }
        if is_section_end(line) {
            // "0 / END OF SYSTEM-WIDE DATA, BEGIN BUS DATA" — skip past it
            pos += 1;
            continue;
        }
        // Settings records (GENERAL, GAUSS, NEWTON, ADJUST …) have non-numeric first tokens
        let first = line
            .split(|c: char| c == ',' || c.is_ascii_whitespace())
            .next()
            .unwrap_or("");
        if first.parse::<f64>().is_err() {
            pos += 1;
            continue;
        }
        break; // reached the first bus record
    }

    // Section 1: Bus Data
    let buses;
    (buses, pos) = parse_bus_section(&lines, pos)?;

    // Build bus base_kv lookup for transformer tap conversion
    let bus_basekv: HashMap<u32, f64> = buses.iter().map(|b| (b.number, b.base_kv)).collect();

    network.buses = buses;

    // Post-parse: fix vmin/vmax that are in kV rather than p.u.
    sanitize_voltage_limits(&mut network);

    // Section 2: Load Data — accumulate into bus Pd/Qd
    let loads;
    (loads, pos) = parse_load_section(&lines, pos)?;
    apply_loads(&mut network, &loads).map_err(|err| PsseError::Parse {
        line: 1,
        message: err.to_string(),
    })?;

    // Section 3: Fixed Shunt Data — accumulate into bus Gs/Bs
    let shunts;
    (shunts, pos) = parse_fixed_shunt_section(&lines, pos)?;
    // PSS/E v36 inserts VOLTAGE DROOP CONTROL DATA between fixed shunts and generators.
    if version >= 36 {
        let droop_controls;
        (droop_controls, pos) = parse_voltage_droop_control_section(&lines, pos);
        network.metadata.voltage_droop_controls = droop_controls;
    }
    pos = skip_to_section(&lines, pos, "generator");
    apply_shunts(&mut network, &shunts);

    // Section 4: Generator Data
    let generators;
    (generators, pos) = parse_generator_section(&lines, pos)?;
    // PSS/E v36 inserts SWITCHING DEVICE RATING SET DATA between generators and branches.
    if version >= 36 {
        let rating_sets;
        (rating_sets, pos) = parse_switching_device_rating_set_section(&lines, pos);
        network.metadata.switching_device_rating_sets = rating_sets;
    }
    pos = skip_to_section(&lines, pos, "branch");
    network.generators = generators;

    // Section 5: Non-Transformer Branch Data
    let branches;
    let branch_terminal_shunts;
    (branches, branch_terminal_shunts, pos) = parse_branch_section(&lines, pos, sbase)?;
    network.branches = branches;
    // Apply GI/BI/GJ/BJ terminal shunts as fixed bus shunts (same as MATPOWER).
    apply_shunts(&mut network, &branch_terminal_shunts);

    // Section 5a: System Switching Device Data (v34+, between branches and transformers).
    // Only present in v34+ files; older files go directly to transformers.
    let sys_switch_devices;
    if version >= 34 {
        if let Some(sys_pos) =
            seek_section(&lines, pos.saturating_sub(1), "system switching device")
        {
            (sys_switch_devices, pos) = parse_system_switching_device_section(&lines, sys_pos);
            pos = skip_to_section(&lines, pos, "transformer");
        } else {
            sys_switch_devices = Vec::new();
        }
    } else {
        sys_switch_devices = Vec::new();
    }

    // Section 6: Transformer Data
    let (transformers, star_buses, oltc_specs, par_specs, pos) =
        parse_transformer_section(&lines, pos, sbase, version, &bus_basekv)?;
    // Add fictitious star buses (from 3-winding transformer expansion) before branches
    // so the Y-bus builder can find them by bus number.
    network.buses.extend(star_buses);
    network.branches.extend(transformers);
    network.controls.oltc_specs.extend(oltc_specs);
    network.controls.par_specs.extend(par_specs);

    // Sections 7–15: Area Interchange, DC Lines, VSC DC, FACTS, Switched Shunts.
    //
    // Back up one line so that seek_section can re-examine the transformer end
    // marker, which in standard PSS/E files doubles as the area-interchange begin
    // marker (e.g. "0 / END OF TRANSFORMER DATA, BEGIN AREA INTERCHANGE DATA").
    // Each seek_section call scans from `seek_start` through ALL remaining lines.
    // We always scan from the same starting point so that each section's
    // begin-marker is independently located regardless of where the previous
    // section's parser left off.
    let seek_start = if pos > 0 { pos - 1 } else { pos };

    // Section 7: Area Interchange Data
    if let Some(ai_pos) = seek_section(&lines, seek_start, "area interchange") {
        let (areas, _) = parse_area_schedule_section(&lines, ai_pos)?;
        network.area_schedules = areas;
    }

    // Section 8: Two-Terminal DC Line Data
    if let Some(dc_pos) = seek_section(&lines, seek_start, "two-terminal dc") {
        let (lcc_links, _) = parse_dc_line_section(&lines, dc_pos)?;
        network.hvdc.links = lcc_links
            .into_iter()
            .map(surge_network::network::HvdcLink::Lcc)
            .collect();
    }

    // Section 9: VSC DC Line Data
    if let Some(vsc_pos) = seek_section(&lines, seek_start, "vsc dc line") {
        let (vsc_lines, _) = parse_vsc_dc_section(&lines, vsc_pos)?;
        network.hvdc.links.extend(
            vsc_lines
                .into_iter()
                .map(surge_network::network::HvdcLink::Vsc),
        );
    }

    // Section 10: Impedance Correction Data
    if let Some(ic_pos) = seek_section(&lines, seek_start, "impedance correction") {
        let (tables, _) = parse_impedance_correction_section(&lines, ic_pos);
        network.metadata.impedance_corrections = tables;
    }

    // Section 11: Multi-Terminal DC Data
    if let Some(mt_pos) = seek_section(&lines, seek_start, "multi-terminal dc") {
        let (mt_lines, _) = parse_multi_terminal_dc_section(&lines, mt_pos);
        normalize_dc_grids(&mut network, &mt_lines);
    }

    // Section 12: Multi-Section Line Data
    if let Some(ms_pos) = seek_section(&lines, seek_start, "multi-section line") {
        let (ms_lines, _) = parse_multi_section_line_section(&lines, ms_pos);
        network.metadata.multi_section_line_groups = ms_lines;
    }

    // Section 13: Zone Data
    if let Some(z_pos) = seek_section(&lines, seek_start, "zone") {
        let (zones, _) = parse_zone_section(&lines, z_pos);
        network.metadata.regions = zones;
    }

    // Section 13b: Inter-Area Transfer Data
    if let Some(ia_pos) = seek_section(&lines, seek_start, "inter-area transfer") {
        let (transfers, _) = parse_inter_area_transfer_section(&lines, ia_pos);
        network.metadata.scheduled_area_transfers = transfers;
    }

    // Section 13c: Owner Data
    if let Some(ow_pos) = seek_section(&lines, seek_start, "owner") {
        let (owners, _) = parse_owner_section(&lines, ow_pos);
        network.metadata.owners = owners;
    }

    // Section 14: FACTS Control Device Data
    // Seek "facts" rather than "facts device" — real PSS/E files use
    // "FACTS CONTROL DEVICE DATA" where "facts device" is not contiguous.
    if let Some(facts_pos) = seek_section(&lines, seek_start, "facts") {
        let (facts, _) = parse_facts_section(&lines, facts_pos);
        network.facts_devices = facts;
    }

    // Section 15: Switched Shunt Data
    // seek_section scans ALL lines (including any intermediate data records) until it
    // finds the "BEGIN SWITCHED SHUNT DATA" marker.
    if let Some(sw_pos) = seek_section(&lines, seek_start, "switched shunt") {
        let (sw_shunts, _) = parse_switched_shunt_section(&lines, sw_pos)?;
        let base_mva = network.base_mva;
        apply_switched_shunts(&mut network, &sw_shunts, base_mva);
    }

    // Section 17+: Induction Machine Data (v35+).
    if let Some(im_pos) = seek_section(&lines, seek_start, "induction machine") {
        let machines = parse_induction_machine_section(&lines, im_pos);
        network.induction_machines = machines;
    }

    // Section 16+: Substation Data (v35+).
    // Contains the full node-breaker topology: substations, nodes, switching devices,
    // and terminal connections.  When present, builds a NodeBreakerTopology on the network.
    if let Some(sub_pos) = seek_section(&lines, seek_start, "substation") {
        let sm = parse_substation_data_section(&lines, sub_pos, &bus_basekv, &sys_switch_devices);
        if !sm.connectivity_nodes.is_empty() {
            network.topology = Some(sm);
        }
    } else if !sys_switch_devices.is_empty() {
        // No SUBSTATION DATA section, but we have system-level switching devices.
        // Build a minimal NodeBreakerTopology from the system switching devices.
        network.topology = Some(build_sys_switch_model(
            &sys_switch_devices,
            &network,
            &bus_basekv,
        ));
    }
    Ok(network)
}

// ---------------------------------------------------------------------------
// Header parsing
// ---------------------------------------------------------------------------

fn parse_header(line: &str, line_num: usize) -> Result<(f64, u32, f64), PsseError> {
    let fields = tokenize_record(line);
    if fields.is_empty() {
        return Err(PsseError::Parse {
            line: line_num,
            message: "empty header line".into(),
        });
    }

    // IC, SBASE, REV, XFRRAT, NXFRAT, BASFRQ / ...
    let sbase = if fields.len() > 1 {
        parse_f64(&fields[1], line_num, "SBASE")?
    } else {
        100.0
    };

    let version = if fields.len() > 2 {
        parse_f64(&fields[2], line_num, "REV")? as u32
    } else {
        33 // default to v33
    };

    // BASFRQ is field index 5 (IC=0, SBASE=1, REV=2, XFRRAT=3, NXFRAT=4, BASFRQ=5).
    // Default to 60 Hz if not present or zero.
    let freq_hz = if fields.len() > 5 {
        let f = parse_f64(&fields[5], line_num, "BASFRQ").unwrap_or(60.0);
        if f > 0.0 { f } else { 60.0 }
    } else {
        60.0
    };

    Ok((sbase, version, freq_hz))
}

// ---------------------------------------------------------------------------
// Section-navigation helper
// ---------------------------------------------------------------------------

/// Skip over unknown "interpolated" PSS/E sections until the section whose
/// `0 / BEGIN … DATA` marker contains `target` (case-insensitive).
///
/// PSS/E v36 inserts extra sections (VOLTAGE DROOP CONTROL DATA,
/// SWITCHING DEVICE RATING SET DATA, SYSTEM SWITCHING DEVICE DATA, …)
/// between the standard sections that older parsers know about.
///
/// After a standard section's parser returns, `pos` points to the first line
/// of the next (possibly unknown) section.  This function advances `pos` past
/// any such intermediate sections by scanning forward for section-end lines
/// that contain the target keyword.
///
/// For standard v33/v34 files with no extra sections the function detects that
/// `pos` is already at real data and returns it unchanged.
fn skip_to_section(lines: &[&str], pos: usize, target: &str) -> usize {
    let target_lc = target.to_ascii_lowercase();
    let mut i = pos;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with("@!") {
            i += 1;
            continue;
        }
        if is_section_end(line) {
            let lc = line.to_ascii_lowercase();
            // Match only when the target keyword appears AFTER "begin" in the
            // line.  Using a simple substring search starting from the "begin"
            // position prevents false matches where the target keyword appears
            // in the "END OF X DATA" prefix but not in the "BEGIN Y DATA"
            // suffix (e.g. "0 / END OF GENERATOR DATA, BEGIN BRANCH DATA"
            // must not match when seeking "generator").
            if let Some(begin_pos) = lc.find("begin")
                && lc[begin_pos..].contains(&target_lc)
            {
                return i + 1;
            }
            // End of an unknown intermediate section (or an empty target
            // section) — keep scanning.
            i += 1;
            continue;
        }
        // Non-blank, non-@!, non-section-end: we are already inside real data.
        // The caller's section parser can handle the @! annotation skip itself.
        break;
    }
    pos // fallback: already positioned at target section
}

/// Scan ALL lines from `start` (including data records in intermediate sections)
/// until a section-end line containing "BEGIN <target>" is found.
///
/// Unlike `skip_to_section`, this does not break on data records, so it correctly
/// locates sections that appear after non-empty intermediate sections (e.g.
/// Switched Shunt Data follows Area Interchange + Two-Terminal DC + FACTS data).
///
/// The target keyword is only matched when it appears AFTER "begin" in the line,
/// preventing false matches with the "END OF X DATA" prefix of combo markers like
/// "0 / END OF AREA INTERCHANGE DATA, BEGIN TWO-TERMINAL DC DATA".
///
/// Returns `Some(first_data_line_pos)` or `None` if the section is not present.
fn seek_section(lines: &[&str], start: usize, target: &str) -> Option<usize> {
    let target_lc = target.to_ascii_lowercase();
    for i in start..lines.len() {
        let line = lines[i].trim();
        if is_section_end(line) {
            let lc = line.to_ascii_lowercase();
            if let Some(begin_pos) = lc.find("begin")
                && lc[begin_pos..].contains(&target_lc)
            {
                return Some(i + 1);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Bus Data
// ---------------------------------------------------------------------------

use crate::parse_utils::{
    RawLoad, RawShunt, RawSwitchedShunt, apply_loads, apply_shunts, apply_switched_shunts,
    sanitize_voltage_limits, unquote,
};

fn parse_bus_section(lines: &[&str], start: usize) -> Result<(Vec<Bus>, usize), PsseError> {
    let mut buses = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();
        if is_section_end(line) {
            return Ok((buses, pos + 1));
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return Ok((buses, pos + 1));
        }
        if line.starts_with("@!") {
            pos += 1;
            continue;
        }

        let fields = tokenize_record(line);
        if fields.is_empty() {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;
        // I, 'NAME', BASKV, IDE, AREA, ZONE, OWNER, VM, VA
        // Minimum fields: I, NAME, BASKV, IDE (4 fields)
        if fields.len() < 4 {
            pos += 1;
            continue;
        }

        let number = parse_f64(&fields[0], line_num, "bus number")? as u32;
        let name = unquote(&fields[1]);
        let base_kv = parse_f64(&fields[2], line_num, "BASKV")?;
        let ide = parse_f64(&fields[3], line_num, "IDE")? as u32;

        let area = if fields.len() > 4 {
            parse_f64(&fields[4], line_num, "AREA")? as u32
        } else {
            1
        };
        let zone = if fields.len() > 5 {
            parse_f64(&fields[5], line_num, "ZONE")? as u32
        } else {
            1
        };
        let owner = if fields.len() > 6 {
            parse_f64(&fields[6], line_num, "OWNER").unwrap_or(1.0) as u32
        } else {
            1
        };
        let vm = if fields.len() > 7 {
            parse_f64(&fields[7], line_num, "VM")?
        } else {
            1.0
        };
        let va_deg = if fields.len() > 8 {
            parse_f64(&fields[8], line_num, "VA")?
        } else {
            0.0
        };

        let bus_type = match ide {
            2 => BusType::PV,
            3 => BusType::Slack,
            4 => BusType::Isolated,
            _ => BusType::PQ,
        };

        // fields 9-10: NVHI, NVLO (normal voltage limits); 11-12: EVHI, EVLO
        let vmax = if fields.len() > 9 {
            parse_f64(&fields[9], line_num, "NVHI").unwrap_or(1.1)
        } else {
            1.1
        };
        let vmin = if fields.len() > 10 {
            parse_f64(&fields[10], line_num, "NVLO").unwrap_or(0.9)
        } else {
            0.9
        };

        // PSS/E v35+ adds LATITUDE and LONGITUDE after NVHI/NVLO/EVHI/EVLO (fields 9–12).
        // Parse them if present; treat 0.0 as absent (no plant is at (0°,0°)).
        let latitude = if fields.len() > 13 {
            parse_f64(&fields[13], line_num, "LATITUDE")
                .ok()
                .filter(|&v| v.abs() > 1e-10)
        } else {
            None
        };
        let longitude = if fields.len() > 14 {
            parse_f64(&fields[14], line_num, "LONGITUDE")
                .ok()
                .filter(|&v| v.abs() > 1e-10)
        } else {
            None
        };

        buses.push(Bus {
            number,
            name,
            bus_type,
            shunt_conductance_mw: 0.0, // will be set from Fixed Shunt section
            shunt_susceptance_mvar: 0.0,
            area,
            voltage_magnitude_pu: vm,
            voltage_angle_rad: va_deg.to_radians(),
            base_kv,
            zone,
            voltage_max_pu: vmax,
            voltage_min_pu: vmin,
            island_id: 0,
            latitude,
            longitude,
            owners: if owner > 0 {
                vec![surge_network::network::OwnershipEntry {
                    owner,
                    fraction: 1.0,
                }]
            } else {
                Vec::new()
            },
            ..Bus::new(0, BusType::PQ, 0.0)
        });

        pos += 1;
    }

    Err(PsseError::UnexpectedEof("Bus Data".into()))
}

// ---------------------------------------------------------------------------
// Load Data
// ---------------------------------------------------------------------------

fn parse_load_section(lines: &[&str], start: usize) -> Result<(Vec<RawLoad>, usize), PsseError> {
    let mut loads = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();
        if is_section_end(line) {
            return Ok((loads, pos + 1));
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return Ok((loads, pos + 1));
        }
        if line.starts_with("@!") {
            pos += 1;
            continue;
        }

        let fields = tokenize_record(line);
        if fields.is_empty() {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;
        // I, ID, STATUS, AREA, ZONE, PL, QL, IP, IQ, YP, YQ, OWNER, SCALE, INTRPT, DGENP, DGENQ
        if fields.len() < 6 {
            pos += 1;
            continue;
        }

        let bus = parse_f64(&fields[0], line_num, "bus")? as u32;
        let id = unquote(&fields[1]);
        // Older v29/v30 files may use text status codes ('A', 'I', 'BL', etc.).
        let status = parse_status(&fields[2], line_num, "STATUS")?;
        // fields[3] = AREA, fields[4] = ZONE (skip)
        let pl = parse_f64(&fields[5], line_num, "PL")?;
        let ql = if fields.len() > 6 {
            parse_f64(&fields[6], line_num, "QL")?
        } else {
            0.0
        };
        // ZIP components: IP (const-current P), IQ (const-current Q),
        // YP (const-admittance P), YQ (const-admittance Q) — all at V=1.0 pu.
        // Collapse to constant-power equivalent (MATPOWER convention: total P = PL+IP+YP).
        let ip = if fields.len() > 7 {
            parse_f64(&fields[7], line_num, "IP").unwrap_or(0.0)
        } else {
            0.0
        };
        let iq = if fields.len() > 8 {
            parse_f64(&fields[8], line_num, "IQ").unwrap_or(0.0)
        } else {
            0.0
        };
        let yp = if fields.len() > 9 {
            parse_f64(&fields[9], line_num, "YP").unwrap_or(0.0)
        } else {
            0.0
        };
        let yq = if fields.len() > 10 {
            parse_f64(&fields[10], line_num, "YQ").unwrap_or(0.0)
        } else {
            0.0
        };
        let p_total = pl + ip + yp;
        let q_total = ql + iq + yq;

        // Compute ZIP fractions. Default to constant-power when total is ~zero.
        let (zip_pz, zip_pi, zip_pp) = if p_total.abs() > 1e-10 {
            (yp / p_total, ip / p_total, pl / p_total)
        } else {
            (0.0, 0.0, 1.0)
        };
        let (zip_qz, zip_qi, zip_qp) = if q_total.abs() > 1e-10 {
            (yq / q_total, iq / q_total, ql / q_total)
        } else {
            (0.0, 0.0, 1.0)
        };

        if ip.abs() > 1e-10 || iq.abs() > 1e-10 || yp.abs() > 1e-10 || yq.abs() > 1e-10 {
            tracing::debug!(
                bus,
                ip,
                iq,
                yp,
                yq,
                "PSS/E load at bus {bus}: ZIP fractions preserved \
                 (Z={zip_pz:.4}/{zip_qz:.4}, I={zip_pi:.4}/{zip_qi:.4}, P={zip_pp:.4}/{zip_qp:.4})"
            );
        }

        let owner = if fields.len() > 11 {
            Some(parse_f64(&fields[11], line_num, "OWNER").unwrap_or(1.0) as u32)
        } else {
            None
        };
        // SCALE field (index 12): 1 = conforming (default), 0 = non-conforming.
        let conforming = if fields.len() > 12 {
            let scale_val = parse_f64(&fields[12], line_num, "SCALE").unwrap_or(1.0);
            scale_val.abs() > 0.5
        } else {
            true
        };

        loads.push(RawLoad {
            bus,
            id,
            status,
            owner,
            pl: p_total,
            ql: q_total,
            conforming,
            zip_p_impedance_frac: zip_pz,
            zip_p_current_frac: zip_pi,
            zip_p_power_frac: zip_pp,
            zip_q_impedance_frac: zip_qz,
            zip_q_current_frac: zip_qi,
            zip_q_power_frac: zip_qp,
        });

        pos += 1;
    }

    Err(PsseError::UnexpectedEof("Load Data".into()))
}

// ---------------------------------------------------------------------------
// Fixed Shunt Data
// ---------------------------------------------------------------------------

fn parse_fixed_shunt_section(
    lines: &[&str],
    start: usize,
) -> Result<(Vec<RawShunt>, usize), PsseError> {
    let mut shunts = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();
        if is_section_end(line) {
            return Ok((shunts, pos + 1));
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return Ok((shunts, pos + 1));
        }
        if line.starts_with("@!") {
            pos += 1;
            continue;
        }

        let fields = tokenize_record(line);
        if fields.is_empty() {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;
        // I, ID, STATUS, GL, BL
        if fields.len() < 5 {
            pos += 1;
            continue;
        }

        let bus = parse_f64(&fields[0], line_num, "bus")? as u32;
        // fields[1] = ID
        // Older v29/v30 files may use text status codes ('A', 'I', 'BL', etc.).
        let status = parse_status(&fields[2], line_num, "STATUS")?;
        let gl = parse_f64(&fields[3], line_num, "GL")?;
        let bl = parse_f64(&fields[4], line_num, "BL")?;

        shunts.push(RawShunt {
            bus,
            status,
            gl,
            bl,
        });

        pos += 1;
    }

    Err(PsseError::UnexpectedEof("Fixed Shunt Data".into()))
}

// ---------------------------------------------------------------------------
// Switched Shunt Data
// ---------------------------------------------------------------------------

/// Parse PSS/E Switched Shunt Data section.
///
/// Record format (v33/v34/v35/v36):
///   I, MODSW, ADJM, STAT, VSWHI, VSWLO, SWREM, RMPCT, RMIDNT, BINIT [, N1, B1, ...]
///
/// For power flow purposes all in-service entries are treated as fixed shunts
/// at their current operating point `BINIT` (Mvar, capacitive positive).
/// Controlled shunts (MODSW ≠ 0) that are not yet at steady state will
/// produce the same small voltage errors as any initialisation mismatch —
/// acceptable for a warm-start power flow.  Full switched-shunt control
/// (discrete or continuous stepping) is a future enhancement.
///
/// Parse the PSS/E SWITCHED SHUNT DATA section into `RawSwitchedShunt` records.
///
/// Returns `(shunts, next_pos)`. Never returns `Err` — individual malformed
/// records are silently skipped so that files with unexpected formats still
/// parse the records they can.
///
/// PSS/E record layout (v30/v33/v34/v35):
/// ```text
/// I, MODSW, ADJM, STAT, VSWHI, VSWLO, SWREM, RMPCT, RMIDNT, BINIT,
/// N1, B1, N2, B2, N3, B3, N4, B4, N5, B5, N6, B6, N7, B7, N8, B8
/// ```
/// Fields 10 onward (N1,B1,...,N8,B8) are optional — older files may omit them.
fn parse_switched_shunt_section(
    lines: &[&str],
    start: usize,
) -> Result<(Vec<RawSwitchedShunt>, usize), PsseError> {
    let mut shunts = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();

        if is_section_end(line) {
            return Ok((shunts, pos + 1));
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return Ok((shunts, pos + 1));
        }
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }

        let fields = tokenize_record(line);
        if fields.is_empty() {
            pos += 1;
            continue;
        }

        // Minimum: I, MODSW, ADJM, STAT, VSWHI, VSWLO, SWREM, RMPCT, RMIDNT, BINIT (10 fields)
        if fields.len() < 10 {
            return Err(PsseError::Parse {
                line: pos + 1,
                message: format!(
                    "switched shunt record is truncated: expected at least 10 fields, got {}",
                    fields.len()
                ),
            });
        }

        let bus = match fields[0].trim_matches('\'').parse::<f64>() {
            Ok(v) => v as u32,
            Err(_) => {
                return Err(PsseError::Parse {
                    line: pos + 1,
                    message: "invalid switched shunt bus number".into(),
                });
            }
        };

        let modsw = fields[1].trim_matches('\'').parse::<f64>().unwrap_or(0.0) as i32;
        // fields[2] = ADJM (adjustment mechanism — not used for PF)
        let stat = match fields[3].trim_matches('\'').parse::<f64>() {
            Ok(v) => v as i32,
            Err(_) => {
                return Err(PsseError::Parse {
                    line: pos + 1,
                    message: "invalid switched shunt status".into(),
                });
            }
        };
        let vswhi = fields[4].trim_matches('\'').parse::<f64>().unwrap_or(1.1);
        let vswlo = fields[5].trim_matches('\'').parse::<f64>().unwrap_or(0.9);
        let swrem = fields[6].trim_matches('\'').parse::<f64>().unwrap_or(0.0) as u32;
        // fields[7] = RMPCT, fields[8] = RMIDNT (quoted identifier string — skip)
        let binit_mvar = match fields[9].trim_matches('\'').parse::<f64>() {
            Ok(v) => v,
            Err(_) => {
                return Err(PsseError::Parse {
                    line: pos + 1,
                    message: "invalid switched shunt BINIT".into(),
                });
            }
        };

        // Parse optional (N, B) step blocks: up to 8 pairs starting at field 10.
        // Each pair is (Ni: i32, Bi: f64) — Bi is in Mvar per step at 1 pu voltage.
        let mut blocks = Vec::new();
        let mut fi = 10usize;
        while fi + 1 < fields.len() && blocks.len() < 8 {
            let ni = fields[fi].trim_matches('\'').parse::<f64>().unwrap_or(0.0) as i32;
            let bi = fields[fi + 1]
                .trim_matches('\'')
                .parse::<f64>()
                .unwrap_or(0.0);
            blocks.push((ni, bi));
            fi += 2;
        }

        shunts.push(RawSwitchedShunt {
            bus,
            modsw,
            stat,
            vswhi,
            vswlo,
            swrem,
            binit: binit_mvar,
            blocks,
        });

        pos += 1;
    }

    Ok((shunts, pos))
}

// ---------------------------------------------------------------------------
// Generator Data
// ---------------------------------------------------------------------------

fn parse_generator_section(
    lines: &[&str],
    start: usize,
) -> Result<(Vec<Generator>, usize), PsseError> {
    let mut generators = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();
        if is_section_end(line) {
            return Ok((generators, pos + 1));
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return Ok((generators, pos + 1));
        }
        if line.starts_with("@!") {
            pos += 1;
            continue;
        }

        let fields = tokenize_record(line);
        if fields.is_empty() {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;
        // I, ID, PG, QG, QT, QB, VS, IREG, MBASE, ZR, ZX, RT, XT, GTAP, STAT, RMPCT, PT, PB, ...
        if fields.len() < 15 {
            pos += 1;
            continue;
        }

        let bus = parse_f64(&fields[0], line_num, "bus")? as u32;
        // fields[1] = ID (machine ID, strip surrounding quotes and whitespace)
        let machine_id: Option<String> = fields.get(1).map(|s| {
            let trimmed = s
                .trim()
                .trim_matches('\'')
                .trim_matches('"')
                .trim()
                .to_string();
            if trimmed.is_empty() {
                "1".to_string()
            } else {
                trimmed
            }
        });
        let pg = parse_f64(&fields[2], line_num, "PG")?;
        let qg = parse_f64(&fields[3], line_num, "QG")?;
        let qt = parse_f64(&fields[4], line_num, "QT")?;
        let qb = parse_f64(&fields[5], line_num, "QB")?;
        let vs = parse_f64(&fields[6], line_num, "VS")?;
        // fields[7] = IREG: remote regulated bus (0 = own terminal bus)
        let reg_bus: Option<u32> = if fields.len() > 7 && !fields[7].trim().is_empty() {
            let ireg = parse_f64(&fields[7], line_num, "IREG").unwrap_or(0.0) as i64;
            if ireg != 0 {
                Some(ireg.unsigned_abs() as u32)
            } else {
                None
            }
        } else {
            None
        };
        let mbase = parse_f64(&fields[8], line_num, "MBASE")?;
        // fields[9] = ZR (armature resistance), fields[10] = ZX (machine leakage reactance / xs)
        let xs = if fields.len() > 10 && !fields[10].trim().is_empty() {
            let zx = parse_f64(&fields[10], line_num, "ZX").unwrap_or(0.0);
            if zx > 0.0 { Some(zx) } else { None }
        } else {
            None
        };
        // fields[11..13] = RT, XT, GTAP
        // STAT: required in the source record; empty or unknown values are errors.
        // Older v29/v30 files may use text status codes ('A', 'I', 'BL', etc.).
        let stat = if fields.len() > 14 {
            parse_status(&fields[14], line_num, "STAT")?
        } else {
            return Err(PsseError::Parse {
                line: line_num,
                message: "missing required generator status (STAT) field".into(),
            });
        };

        let mut pt = if fields.len() > 16 && !fields[16].trim().is_empty() {
            parse_f64(&fields[16], line_num, "PT")?
        } else {
            9999.0 // PSS/E convention for "unlimited"
        };
        let mut pb = if fields.len() > 17 {
            parse_f64(&fields[17], line_num, "PB")?
        } else {
            0.0
        };

        // Sanity-check pmax/pmin.  PSS/E PTIv33 files often store 0.0 for PT/PB
        // when the data was not explicitly set.  Infer sensible bounds from pg.
        if pt < 0.0 {
            // Negative pmax is physically impossible — take absolute value.
            pt = pt.abs();
        }
        if pt < pg && pg > 0.0 {
            // pmax below the scheduled output: infer pmax = pg * 1.1 (10% headroom).
            pt = pg * 1.1;
        }
        if pt == 0.0 && pg > 0.0 {
            // pmax == 0 with non-zero scheduled output: the PT field was not set in the
            // PSS/E file.  Use 9999 MW (the PSS/E convention for "unlimited") rather than
            // inventing a value derived from the current dispatch.
            tracing::warn!(
                "PSS/E generator at bus {bus}: PT=0 with PG={pg:.1} MW; \
                 setting Pmax=9999 MW (field not provided in source file)"
            );
            pt = 9999.0;
        }
        // pmin must not exceed pmax.
        if pb > pt {
            pb = 0.0;
        }

        generators.push(Generator {
            bus,
            machine_id,
            p: pg,
            q: qg,
            qmax: qt,
            qmin: qb,
            voltage_setpoint_pu: vs,
            reg_bus,
            machine_base_mva: mbase,
            pmax: pt,
            pmin: pb,
            in_service: stat > 0,
            cost: None,
            forced_outage_rate: None,
            agc_participation_factor: None,
            h_inertia_s: None,
            pfr_eligible: true,
            fault_data: xs.map(|xs_val| surge_network::network::GenFaultData {
                xs: Some(xs_val),
                ..Default::default()
            }),
            owners: parse_multi_owner_fields(&fields, 18),
            ..Generator::new(0, 0.0, 1.0)
        });

        pos += 1;
    }

    Err(PsseError::UnexpectedEof("Generator Data".into()))
}

// ---------------------------------------------------------------------------
// Non-Transformer Branch Data
// ---------------------------------------------------------------------------

fn parse_branch_section(
    lines: &[&str],
    start: usize,
    sbase: f64,
) -> Result<(Vec<Branch>, Vec<RawShunt>, usize), PsseError> {
    let mut branches = Vec::new();
    let mut terminal_shunts: Vec<RawShunt> = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();
        if is_section_end(line) {
            return Ok((branches, terminal_shunts, pos + 1));
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return Ok((branches, terminal_shunts, pos + 1));
        }
        if line.starts_with("@!") {
            pos += 1;
            continue;
        }

        let fields = tokenize_record(line);
        if fields.is_empty() {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;
        // I, J, CKT, R, X, B, RATEA, RATEB, RATEC, GI, BI, GJ, BJ, ST, MET, LEN, ...
        if fields.len() < 14 {
            pos += 1;
            continue;
        }

        let from_bus = parse_f64(&fields[0], line_num, "I")? as u32;
        let to_bus = (parse_f64(&fields[1], line_num, "J")? as i64).unsigned_abs() as u32;
        let circuit = unquote(&fields[2]);
        let r = parse_f64(&fields[3], line_num, "R")?;
        let x = parse_f64(&fields[4], line_num, "X")?;
        let b = parse_f64(&fields[5], line_num, "B")?;
        let rate_a = if fields.len() > 6 {
            parse_f64(&fields[6], line_num, "RATEA")?
        } else {
            0.0
        };
        let rate_b = if fields.len() > 7 {
            parse_f64(&fields[7], line_num, "RATEB")?
        } else {
            0.0
        };
        let rate_c = if fields.len() > 8 {
            parse_f64(&fields[8], line_num, "RATEC")?
        } else {
            0.0
        };
        // GI, BI, GJ, BJ: terminal shunt admittances in pu — accumulate to bus shunts.
        // Branch struct has no per-end shunt fields; warn when non-zero so users know.
        let gi = if fields.len() > 9 {
            parse_f64(&fields[9], line_num, "GI")?
        } else {
            0.0
        };
        let bi = if fields.len() > 10 {
            parse_f64(&fields[10], line_num, "BI")?
        } else {
            0.0
        };
        let gj = if fields.len() > 11 {
            parse_f64(&fields[11], line_num, "GJ")?
        } else {
            0.0
        };
        let bj = if fields.len() > 12 {
            parse_f64(&fields[12], line_num, "BJ")?
        } else {
            0.0
        };
        // GI/BI apply at the from-bus terminal; GJ/BJ at the to-bus terminal.
        // Accumulate as fixed bus shunts — identical to MATPOWER's treatment.
        if gi.abs() > 1e-12 || bi.abs() > 1e-12 {
            terminal_shunts.push(RawShunt {
                bus: from_bus,
                status: 1,
                gl: gi * sbase,
                bl: bi * sbase,
            });
        }
        if gj.abs() > 1e-12 || bj.abs() > 1e-12 {
            terminal_shunts.push(RawShunt {
                bus: to_bus,
                status: 1,
                gl: gj * sbase,
                bl: bj * sbase,
            });
        }
        let st = if fields.len() > 13 {
            parse_status(&fields[13], line_num, "ST")?
        } else {
            1
        };

        branches.push(Branch {
            from_bus,
            to_bus,
            circuit,
            r,
            x,
            b,
            rating_a_mva: rate_a,
            rating_b_mva: rate_b,
            rating_c_mva: rate_c,
            tap: 1.0,
            phase_shift_rad: 0.0,
            in_service: st > 0,
            angle_diff_min_rad: None,
            angle_diff_max_rad: None,
            g_pi: 0.0,
            g_mag: 0.0,
            b_mag: 0.0,
            tab: None,
            owners: parse_multi_owner_fields(&fields, 16),
            ..Branch::default()
        });

        pos += 1;
    }

    Err(PsseError::UnexpectedEof("Branch Data".into()))
}

// ---------------------------------------------------------------------------
// Transformer Data
// ---------------------------------------------------------------------------

/// Apply CZ impedance conversion for a transformer winding pair impedance.
///
/// Returns `(r_pu, x_pu)` on the system MVA base.
pub(crate) fn apply_cz_conversion(
    r: f64,
    x: f64,
    mut sbase_winding: f64,
    sbase_sys: f64,
    cz: u32,
) -> (f64, f64) {
    // PE-03: guard against division-by-zero when sbase_winding is 0 (malformed records).
    // Fall back to sbase_sys so the impedance is treated as already on the system base.
    if sbase_winding.abs() < 1e-10 {
        tracing::warn!(
            sbase_winding,
            sbase_sys,
            cz,
            "transformer sbase_winding is zero; falling back to sbase_sys to avoid NaN"
        );
        sbase_winding = sbase_sys;
    }
    // If sbase_sys is also zero (pathological case), return passthrough to avoid NaN.
    if sbase_sys.abs() < 1e-10 {
        return (0.0, 0.0);
    }
    match cz {
        1 => {
            // R/X already in p.u. on system base — use directly
            (r, x)
        }
        2 => {
            // R/X in p.u. on winding base MVA — convert to system base
            if (sbase_winding - sbase_sys).abs() > 1e-10 {
                (r * sbase_sys / sbase_winding, x * sbase_sys / sbase_winding)
            } else {
                (r, x)
            }
        }
        3 => {
            // R in watts (load loss), X in impedance magnitude percent on winding base.
            // R_pu = R_watts / (1e6 * sbase_winding), then scale to system base.
            let r_pu = r / (1_000_000.0 * sbase_winding);
            let x_mag = x / 100.0; // from percent
            let x_pu = if x_mag * x_mag > r_pu * r_pu {
                (x_mag * x_mag - r_pu * r_pu).sqrt()
            } else {
                x_mag
            };
            (
                r_pu * sbase_sys / sbase_winding,
                x_pu * sbase_sys / sbase_winding,
            )
        }
        _ => (r, x),
    }
}

/// Compute a single winding tap in p.u. of the bus base kV.
///
/// `windv`: the WINDV value from the PSS/E record.
/// `nomv`: the NOMV value (kV); 0 means "use bus base kV".
/// `bus_bkv`: the bus base kV from the Bus Data section.
/// `cw`: CW code (1=pu of bkv, 2=kV, 3=pu of nomv).
pub(crate) fn compute_winding_tap_pu(windv: f64, nomv: f64, bus_bkv: f64, cw: u32) -> f64 {
    match cw {
        1 => windv, // already in p.u. of bus base kV
        2 => {
            // WINDV in kV — normalise by bus base kV
            if bus_bkv > 0.0 {
                windv / bus_bkv
            } else {
                windv
            }
        }
        3 => {
            // WINDV in p.u. of NOMV; NOMV in kV (0 means use bus base kV)
            let n = if nomv > 0.0 { nomv } else { bus_bkv };
            if bus_bkv > 0.0 {
                windv * n / bus_bkv
            } else {
                windv
            }
        }
        _ => windv,
    }
}

/// Build a Branch struct literal with all required fields defaulted for a transformer winding.
#[allow(clippy::too_many_arguments)]
pub(crate) fn make_xfmr_branch(
    from_bus: u32,
    to_bus: u32,
    circuit: String,
    r: f64,
    x: f64,
    rating_a_mva: f64,
    rating_b_mva: f64,
    rating_c_mva: f64,
    tap: f64,
    phase_shift_rad: f64,
    in_service: bool,
    g_mag: f64,
    b_mag: f64,
) -> Branch {
    Branch {
        from_bus,
        to_bus,
        circuit,
        r,
        x: if x.abs() < 1e-10 {
            if x < 0.0 { -1e-6 } else { 1e-6 }
        } else {
            x
        },
        b: 0.0,
        rating_a_mva,
        rating_b_mva,
        rating_c_mva,
        tap,
        phase_shift_rad: phase_shift_rad.to_radians(),
        in_service,
        angle_diff_min_rad: None,
        angle_diff_max_rad: None,
        g_pi: 0.0,
        g_mag,
        b_mag,
        tab: None,
        branch_type: BranchType::Transformer,
        ..Branch::default()
    }
}

/// Apply discrete tap/phase step data from per-winding COD/RMA/RMI/NTP fields to a Branch.
fn apply_3w_step_data(br: &mut Branch, cod: i32, rma: f64, rmi: f64, ntp: u32) {
    let range = (rma - rmi).abs();
    let n = if ntp > 0 { ntp as f64 } else { 32.0 };
    match cod.abs() {
        1 | 2 => {
            let ts = range / n;
            if ts > 1e-9 {
                let ctrl = br.opf_control.get_or_insert_with(BranchOpfControl::default);
                ctrl.tap_step = ts;
                ctrl.tap_min = rmi.min(rma);
                ctrl.tap_max = rmi.max(rma);
            }
        }
        3 => {
            let sd = range / n;
            if sd > 1e-9 {
                let ctrl = br.opf_control.get_or_insert_with(BranchOpfControl::default);
                ctrl.phase_step_rad = sd.to_radians();
                ctrl.phase_min_rad = rmi.min(rma).to_radians();
                ctrl.phase_max_rad = rmi.max(rma).to_radians();
            }
        }
        _ => {}
    }
}

type TransformerSectionResult = (Vec<Branch>, Vec<Bus>, Vec<OltcSpec>, Vec<ParSpec>, usize);

fn parse_transformer_section(
    lines: &[&str],
    start: usize,
    sbase: f64,
    _version: u32,
    bus_basekv: &HashMap<u32, f64>,
) -> Result<TransformerSectionResult, PsseError> {
    let mut transformers: Vec<Branch> = Vec::new();
    let mut star_buses: Vec<Bus> = Vec::new();
    let mut oltc_specs: Vec<OltcSpec> = Vec::new();
    let mut par_specs: Vec<ParSpec> = Vec::new();
    let mut pos = start;

    // Assign unique bus numbers to fictitious 3-winding star buses beyond all
    // existing bus numbers.
    let mut max_bus_num: u32 = bus_basekv.keys().copied().max().unwrap_or(0);

    while pos < lines.len() {
        let line = lines[pos].trim();
        if is_section_end(line) {
            return Ok((transformers, star_buses, oltc_specs, par_specs, pos + 1));
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return Ok((transformers, star_buses, oltc_specs, par_specs, pos + 1));
        }
        if line.starts_with("@!") {
            pos += 1;
            continue;
        }

        // Record 1: I, J, K, CKT, CW, CZ, CM, MAG1, MAG2, NMETR, 'NAME', STAT, ...
        let rec1_fields = tokenize_record(line);
        if rec1_fields.is_empty() {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;
        if rec1_fields.len() < 12 {
            return Err(PsseError::Parse {
                line: line_num,
                message: "truncated transformer record 1".into(),
            });
        }

        let from_bus = parse_f64(&rec1_fields[0], line_num, "I")? as u32;
        let to_bus = (parse_f64(&rec1_fields[1], line_num, "J")? as i64).unsigned_abs() as u32;
        let k = parse_f64(&rec1_fields[2], line_num, "K")? as i64;
        let circuit = unquote(&rec1_fields[3]);
        let cw = if rec1_fields.len() > 4 {
            parse_f64(&rec1_fields[4], line_num, "CW")? as u32
        } else {
            1
        };
        let cz = if rec1_fields.len() > 5 {
            parse_f64(&rec1_fields[5], line_num, "CZ")? as u32
        } else {
            1
        };
        // fields[6]=CM, [7]=MAG1 (magnetizing conductance pu), [8]=MAG2 (magnetizing susceptance pu)
        let mag1 = if rec1_fields.len() > 7 {
            parse_f64(&rec1_fields[7], line_num, "MAG1").unwrap_or(0.0)
        } else {
            0.0
        };
        let mag2 = if rec1_fields.len() > 8 {
            parse_f64(&rec1_fields[8], line_num, "MAG2").unwrap_or(0.0)
        } else {
            0.0
        };
        // fields[9]=NMETR, [10]=NAME
        let stat = if rec1_fields.len() > 11 {
            parse_status(&rec1_fields[11], line_num, "STAT")?
        } else {
            return Err(PsseError::Parse {
                line: line_num,
                message: "missing required transformer status (STAT) field".into(),
            });
        };

        let xfmr_owners = parse_multi_owner_fields(&rec1_fields, 12);

        let is_3winding = k != 0;
        let k_bus = k.unsigned_abs() as u32;

        // Record 2: R1-2, X1-2, SBASE1-2 [, R2-3, X2-3, SBASE2-3, R3-1, X3-1, SBASE3-1, VMSTAR, ANSTAR]
        pos += 1;
        if pos >= lines.len() {
            return Err(PsseError::UnexpectedEof("Transformer Record 2".into()));
        }
        // PE-02: if the next line is already a section terminator the multi-line
        // transformer record was truncated; skip this transformer to avoid consuming
        // lines that belong to the next section.
        {
            let peek = lines[pos].trim();
            if is_section_end(peek) || peek.starts_with('Q') || peek.eq_ignore_ascii_case("q") {
                tracing::warn!(
                    line = pos + 1,
                    from_bus,
                    to_bus,
                    "Truncated transformer Record 2 at line {}; skipping transformer.",
                    pos + 1
                );
                return Err(PsseError::UnexpectedEof("Transformer Record 2".into()));
            }
        }
        let rec2_fields = tokenize_record(lines[pos].trim());
        let line_num2 = pos + 1;

        let r12_raw = if !rec2_fields.is_empty() {
            parse_f64(&rec2_fields[0], line_num2, "R1-2")?
        } else {
            0.0
        };
        let x12_raw = if rec2_fields.len() > 1 {
            parse_f64(&rec2_fields[1], line_num2, "X1-2")?
        } else {
            0.01
        };
        let sbase12 = if rec2_fields.len() > 2 {
            parse_f64(&rec2_fields[2], line_num2, "SBASE1-2")?
        } else {
            sbase
        };

        let (r12, x12) = apply_cz_conversion(r12_raw, x12_raw, sbase12, sbase, cz);

        if is_3winding {
            tracing::warn!(
                from_bus,
                to_bus,
                k_bus,
                "3-winding transformer modeled via star (Y) bus expansion; \
                 fictitious internal bus inserted, tap ratios and magnetizing \
                 admittance applied to winding 1 only"
            );
            // ---------------------------------------------------------------
            // 3-winding transformer: star (Y) topology expansion
            // ---------------------------------------------------------------
            // Parse additional winding pairs from Record 2 (3-winding format):
            //   R2-3, X2-3, SBASE2-3, R3-1, X3-1, SBASE3-1, VMSTAR, ANSTAR
            let r23_raw = if rec2_fields.len() > 3 {
                parse_f64(&rec2_fields[3], line_num2, "R2-3").unwrap_or(0.0)
            } else {
                0.0
            };
            let x23_raw = if rec2_fields.len() > 4 {
                parse_f64(&rec2_fields[4], line_num2, "X2-3").unwrap_or(0.01)
            } else {
                0.01
            };
            let sbase23 = if rec2_fields.len() > 5 {
                parse_f64(&rec2_fields[5], line_num2, "SBASE2-3").unwrap_or(sbase)
            } else {
                sbase
            };
            let r31_raw = if rec2_fields.len() > 6 {
                parse_f64(&rec2_fields[6], line_num2, "R3-1").unwrap_or(0.0)
            } else {
                0.0
            };
            let x31_raw = if rec2_fields.len() > 7 {
                parse_f64(&rec2_fields[7], line_num2, "X3-1").unwrap_or(0.01)
            } else {
                0.01
            };
            let sbase31 = if rec2_fields.len() > 8 {
                parse_f64(&rec2_fields[8], line_num2, "SBASE3-1").unwrap_or(sbase)
            } else {
                sbase
            };
            let vmstar = if rec2_fields.len() > 9 {
                parse_f64(&rec2_fields[9], line_num2, "VMSTAR").unwrap_or(1.0)
            } else {
                1.0
            };
            let anstar_deg = if rec2_fields.len() > 10 {
                parse_f64(&rec2_fields[10], line_num2, "ANSTAR").unwrap_or(0.0)
            } else {
                0.0
            };

            let (r23, x23) = apply_cz_conversion(r23_raw, x23_raw, sbase23, sbase, cz);
            let (r31, x31) = apply_cz_conversion(r31_raw, x31_raw, sbase31, sbase, cz);

            // Star-delta impedance conversion:
            //   Z1 = (Z12 + Z31 - Z23) / 2
            //   Z2 = (Z12 + Z23 - Z31) / 2
            //   Z3 = (Z23 + Z31 - Z12) / 2
            let r1 = (r12 + r31 - r23) / 2.0;
            let x1 = (x12 + x31 - x23) / 2.0;
            let r2 = (r12 + r23 - r31) / 2.0;
            let x2 = (x12 + x23 - x31) / 2.0;
            let r3 = (r23 + r31 - r12) / 2.0;
            let x3 = (x23 + x31 - x12) / 2.0;

            // Record 3: winding 1 (I bus — from_bus)
            pos += 1;
            if pos >= lines.len() {
                return Err(PsseError::UnexpectedEof("Transformer Record 3 (3W)".into()));
            }
            // PE-02: truncated 3W record — section end encountered before Record 3.
            {
                let peek = lines[pos].trim();
                if is_section_end(peek) || peek.starts_with('Q') || peek.eq_ignore_ascii_case("q") {
                    tracing::warn!(
                        line = pos + 1,
                        from_bus,
                        to_bus,
                        k_bus,
                        "Truncated 3W transformer Record 3 at line {}; skipping transformer.",
                        pos + 1
                    );
                    return Err(PsseError::UnexpectedEof("Transformer Record 3 (3W)".into()));
                }
            }
            let rec3_fields = tokenize_record(lines[pos].trim());
            let line_num3 = pos + 1;
            let windv1 = if !rec3_fields.is_empty() {
                parse_f64(&rec3_fields[0], line_num3, "WINDV1")?
            } else {
                1.0
            };
            let nomv1 = if rec3_fields.len() > 1 {
                parse_f64(&rec3_fields[1], line_num3, "NOMV1").unwrap_or(0.0)
            } else {
                0.0
            };
            let ang1 = if rec3_fields.len() > 2 {
                parse_f64(&rec3_fields[2], line_num3, "ANG1").unwrap_or(0.0)
            } else {
                0.0
            };
            let rata1 = if rec3_fields.len() > 3 {
                parse_f64(&rec3_fields[3], line_num3, "RATA1").unwrap_or(0.0)
            } else {
                0.0
            };
            let ratb1 = if rec3_fields.len() > 4 {
                parse_f64(&rec3_fields[4], line_num3, "RATB1").unwrap_or(0.0)
            } else {
                0.0
            };
            let ratc1 = if rec3_fields.len() > 5 {
                parse_f64(&rec3_fields[5], line_num3, "RATC1").unwrap_or(0.0)
            } else {
                0.0
            };

            // Parse winding-1 control fields (COD1/NTP1/RMA1/RMI1) from Record 3.
            // Same layout as 2W Record 3: fields [6..12].
            let cod1_3w = if rec3_fields.len() > 6 {
                parse_f64(&rec3_fields[6], line_num3, "COD1").unwrap_or(0.0) as i32
            } else {
                0
            };
            let rma1_3w = if rec3_fields.len() > 8 {
                parse_f64(&rec3_fields[8], line_num3, "RMA1").unwrap_or(1.1)
            } else {
                1.1
            };
            let rmi1_3w = if rec3_fields.len() > 9 {
                parse_f64(&rec3_fields[9], line_num3, "RMI1").unwrap_or(0.9)
            } else {
                0.9
            };
            let ntp1_3w = if rec3_fields.len() > 12 {
                parse_f64(&rec3_fields[12], line_num3, "NTP1").unwrap_or(33.0) as u32
            } else {
                33
            };

            // Record 4: winding 2 (J bus — to_bus)
            pos += 1;
            if pos >= lines.len() {
                return Err(PsseError::UnexpectedEof("Transformer Record 4 (3W)".into()));
            }
            // PE-02: truncated 3W record — section end encountered before Record 4.
            {
                let peek = lines[pos].trim();
                if is_section_end(peek) || peek.starts_with('Q') || peek.eq_ignore_ascii_case("q") {
                    tracing::warn!(
                        line = pos + 1,
                        from_bus,
                        to_bus,
                        k_bus,
                        "Truncated 3W transformer Record 4 at line {}; skipping transformer.",
                        pos + 1
                    );
                    return Err(PsseError::UnexpectedEof("Transformer Record 4 (3W)".into()));
                }
            }
            let rec4_fields = tokenize_record(lines[pos].trim());
            let line_num4 = pos + 1;
            let windv2 = if !rec4_fields.is_empty() {
                parse_f64(&rec4_fields[0], line_num4, "WINDV2")?
            } else {
                1.0
            };
            let nomv2 = if rec4_fields.len() > 1 {
                parse_f64(&rec4_fields[1], line_num4, "NOMV2").unwrap_or(0.0)
            } else {
                0.0
            };
            let ang2 = if rec4_fields.len() > 2 {
                parse_f64(&rec4_fields[2], line_num4, "ANG2").unwrap_or(0.0)
            } else {
                0.0
            };
            let rata2 = if rec4_fields.len() > 3 {
                parse_f64(&rec4_fields[3], line_num4, "RATA2").unwrap_or(0.0)
            } else {
                0.0
            };
            let ratb2 = if rec4_fields.len() > 4 {
                parse_f64(&rec4_fields[4], line_num4, "RATB2").unwrap_or(0.0)
            } else {
                0.0
            };
            let ratc2 = if rec4_fields.len() > 5 {
                parse_f64(&rec4_fields[5], line_num4, "RATC2").unwrap_or(0.0)
            } else {
                0.0
            };
            // Winding-2 control fields (COD2/NTP2/RMA2/RMI2).
            let cod2_3w = if rec4_fields.len() > 6 {
                parse_f64(&rec4_fields[6], line_num4, "COD2").unwrap_or(0.0) as i32
            } else {
                0
            };
            let rma2_3w = if rec4_fields.len() > 8 {
                parse_f64(&rec4_fields[8], line_num4, "RMA2").unwrap_or(1.1)
            } else {
                1.1
            };
            let rmi2_3w = if rec4_fields.len() > 9 {
                parse_f64(&rec4_fields[9], line_num4, "RMI2").unwrap_or(0.9)
            } else {
                0.9
            };
            let ntp2_3w = if rec4_fields.len() > 12 {
                parse_f64(&rec4_fields[12], line_num4, "NTP2").unwrap_or(33.0) as u32
            } else {
                33
            };

            // Record 5: winding 3 (K bus — k_bus)
            pos += 1;
            if pos >= lines.len() {
                return Err(PsseError::UnexpectedEof("Transformer Record 5 (3W)".into()));
            }
            // PE-02: truncated 3W record — section end encountered before Record 5.
            {
                let peek = lines[pos].trim();
                if is_section_end(peek) || peek.starts_with('Q') || peek.eq_ignore_ascii_case("q") {
                    tracing::warn!(
                        line = pos + 1,
                        from_bus,
                        to_bus,
                        k_bus,
                        "Truncated 3W transformer Record 5 at line {}; skipping transformer.",
                        pos + 1
                    );
                    return Err(PsseError::UnexpectedEof("Transformer Record 5 (3W)".into()));
                }
            }
            let rec5_fields = tokenize_record(lines[pos].trim());
            let line_num5 = pos + 1;
            let windv3 = if !rec5_fields.is_empty() {
                parse_f64(&rec5_fields[0], line_num5, "WINDV3")?
            } else {
                1.0
            };
            let nomv3 = if rec5_fields.len() > 1 {
                parse_f64(&rec5_fields[1], line_num5, "NOMV3").unwrap_or(0.0)
            } else {
                0.0
            };
            let ang3 = if rec5_fields.len() > 2 {
                parse_f64(&rec5_fields[2], line_num5, "ANG3").unwrap_or(0.0)
            } else {
                0.0
            };
            let rata3 = if rec5_fields.len() > 3 {
                parse_f64(&rec5_fields[3], line_num5, "RATA3").unwrap_or(0.0)
            } else {
                0.0
            };
            let ratb3 = if rec5_fields.len() > 4 {
                parse_f64(&rec5_fields[4], line_num5, "RATB3").unwrap_or(0.0)
            } else {
                0.0
            };
            let ratc3 = if rec5_fields.len() > 5 {
                parse_f64(&rec5_fields[5], line_num5, "RATC3").unwrap_or(0.0)
            } else {
                0.0
            };
            // Winding-3 control fields (COD3/NTP3/RMA3/RMI3).
            let cod3_3w = if rec5_fields.len() > 6 {
                parse_f64(&rec5_fields[6], line_num5, "COD3").unwrap_or(0.0) as i32
            } else {
                0
            };
            let rma3_3w = if rec5_fields.len() > 8 {
                parse_f64(&rec5_fields[8], line_num5, "RMA3").unwrap_or(1.1)
            } else {
                1.1
            };
            let rmi3_3w = if rec5_fields.len() > 9 {
                parse_f64(&rec5_fields[9], line_num5, "RMI3").unwrap_or(0.9)
            } else {
                0.9
            };
            let ntp3_3w = if rec5_fields.len() > 12 {
                parse_f64(&rec5_fields[12], line_num5, "NTP3").unwrap_or(33.0) as u32
            } else {
                33
            };

            // Compute individual winding taps in p.u. of their respective bus base kV
            let bkv1 = bus_basekv.get(&from_bus).copied().unwrap_or(1.0);
            let bkv2 = bus_basekv.get(&to_bus).copied().unwrap_or(1.0);
            let bkv3 = bus_basekv.get(&k_bus).copied().unwrap_or(1.0);
            let tap1 = compute_winding_tap_pu(windv1, nomv1, bkv1, cw);
            let tap2 = compute_winding_tap_pu(windv2, nomv2, bkv2, cw);
            let tap3 = compute_winding_tap_pu(windv3, nomv3, bkv3, cw);

            // Create fictitious star bus
            max_bus_num += 1;
            let star_bus_num = max_bus_num;
            star_buses.push(Bus {
                number: star_bus_num,
                name: format!("STAR_{from_bus}_{to_bus}_{k_bus}"),
                bus_type: BusType::PQ,
                shunt_conductance_mw: 0.0,
                shunt_susceptance_mvar: 0.0,
                area: 1,
                voltage_magnitude_pu: vmstar,
                voltage_angle_rad: anstar_deg.to_radians(),
                base_kv: bkv1.max(bkv2).max(bkv3).max(1.0), // fictitious node: use highest winding kV to avoid div-by-zero in fault analysis
                zone: 1,
                voltage_max_pu: 1.1,
                voltage_min_pu: 0.9,
                island_id: 0,
                latitude: None,
                longitude: None,
                ..Bus::new(0, BusType::PQ, 0.0)
            });

            let in_service = stat > 0;

            // Branch I (from_bus) → star: winding-1 impedance, tap1, shift=ang1
            // Magnetizing admittance (MAG1/MAG2) applied at winding-1 terminal.
            let mut w1 = make_xfmr_branch(
                from_bus,
                star_bus_num,
                circuit.clone(),
                r1,
                x1,
                rata1,
                ratb1,
                ratc1,
                tap1,
                ang1,
                in_service,
                mag1,
                mag2,
            );
            w1.branch_type = BranchType::Transformer3W;
            w1.owners = xfmr_owners.clone();
            apply_3w_step_data(&mut w1, cod1_3w, rma1_3w, rmi1_3w, ntp1_3w);
            transformers.push(w1);

            // Branch J (to_bus) → star: winding-2 impedance, tap2, shift=ang2
            let mut w2 = make_xfmr_branch(
                to_bus,
                star_bus_num,
                circuit.clone(),
                r2,
                x2,
                rata2,
                ratb2,
                ratc2,
                tap2,
                ang2,
                in_service,
                0.0,
                0.0,
            );
            w2.branch_type = BranchType::Transformer3W;
            w2.owners = xfmr_owners.clone();
            apply_3w_step_data(&mut w2, cod2_3w, rma2_3w, rmi2_3w, ntp2_3w);
            transformers.push(w2);

            // Branch K (k_bus) → star: winding-3 impedance, tap3, shift=ang3
            let mut w3 = make_xfmr_branch(
                k_bus,
                star_bus_num,
                circuit,
                r3,
                x3,
                rata3,
                ratb3,
                ratc3,
                tap3,
                ang3,
                in_service,
                0.0,
                0.0,
            );
            w3.branch_type = BranchType::Transformer3W;
            w3.owners = xfmr_owners.clone();
            apply_3w_step_data(&mut w3, cod3_3w, rma3_3w, rmi3_3w, ntp3_3w);
            transformers.push(w3);

            tracing::debug!(
                from_bus,
                to_bus,
                k_bus,
                star_bus = star_bus_num,
                "3-winding transformer expanded to star topology"
            );
        } else {
            // ---------------------------------------------------------------
            // 2-winding transformer
            // ---------------------------------------------------------------

            // Record 3: WINDV1, NOMV1, ANG1, RATA1, RATB1, RATC1, ...
            pos += 1;
            if pos >= lines.len() {
                return Err(PsseError::UnexpectedEof("Transformer Record 3".into()));
            }
            // PE-02: truncated 2W record — section end encountered before Record 3.
            {
                let peek = lines[pos].trim();
                if is_section_end(peek) || peek.starts_with('Q') || peek.eq_ignore_ascii_case("q") {
                    tracing::warn!(
                        line = pos + 1,
                        from_bus,
                        to_bus,
                        "Truncated 2W transformer Record 3 at line {}; skipping transformer.",
                        pos + 1
                    );
                    return Err(PsseError::UnexpectedEof("Transformer Record 3".into()));
                }
            }
            let rec3_fields = tokenize_record(lines[pos].trim());
            let line_num3 = pos + 1;

            let windv1 = if !rec3_fields.is_empty() {
                parse_f64(&rec3_fields[0], line_num3, "WINDV1")?
            } else {
                1.0
            };
            let nomv1 = if rec3_fields.len() > 1 {
                parse_f64(&rec3_fields[1], line_num3, "NOMV1")?
            } else {
                0.0
            };
            let ang1 = if rec3_fields.len() > 2 {
                parse_f64(&rec3_fields[2], line_num3, "ANG1")?
            } else {
                0.0
            };
            let rata1 = if rec3_fields.len() > 3 {
                parse_f64(&rec3_fields[3], line_num3, "RATA1")?
            } else {
                0.0
            };
            let ratb1 = if rec3_fields.len() > 4 {
                parse_f64(&rec3_fields[4], line_num3, "RATB1")?
            } else {
                0.0
            };
            let ratc1 = if rec3_fields.len() > 5 {
                parse_f64(&rec3_fields[5], line_num3, "RATC1")?
            } else {
                0.0
            };
            // Record 3 fields: ... COD1[6], CONT1[7], RMA1[8], RMI1[9], VMA1[10], VMI1[11], NTP1[12], TAB1[13]
            let cod1 = if rec3_fields.len() > 6 {
                parse_f64(&rec3_fields[6], line_num3, "COD1").unwrap_or(0.0) as i32
            } else {
                0
            };
            let cont1 = if rec3_fields.len() > 7 {
                parse_f64(&rec3_fields[7], line_num3, "CONT1").unwrap_or(0.0) as i32
            } else {
                0
            };
            let rma1 = if rec3_fields.len() > 8 {
                parse_f64(&rec3_fields[8], line_num3, "RMA1").unwrap_or(1.1)
            } else {
                1.1
            };
            let rmi1 = if rec3_fields.len() > 9 {
                parse_f64(&rec3_fields[9], line_num3, "RMI1").unwrap_or(0.9)
            } else {
                0.9
            };
            let vma1 = if rec3_fields.len() > 10 {
                parse_f64(&rec3_fields[10], line_num3, "VMA1").unwrap_or(1.1)
            } else {
                1.1
            };
            let vmi1 = if rec3_fields.len() > 11 {
                parse_f64(&rec3_fields[11], line_num3, "VMI1").unwrap_or(0.9)
            } else {
                0.9
            };
            let ntp1 = if rec3_fields.len() > 12 {
                parse_f64(&rec3_fields[12], line_num3, "NTP1").unwrap_or(33.0) as u32
            } else {
                33
            };

            // Build OLTC / PAR specs from COD1 control code.
            //   COD1 = 1,2   → OLTC voltage/reactive control
            //   COD1 = 3     → PAR active power flow control
            //   COD1 = 0,-1  → fixed tap / load-drop (no outer-loop spec needed)
            match cod1 {
                1 | 2 | -1 | -2 => {
                    // Voltage-magnitude or reactive-power control (OLTC).
                    // CONT1 is the regulated bus (negative = remote, positive = local,
                    // 0 = from-bus default; PSS/E convention: negative means from-bus side).
                    let regulated_bus = cont1.unsigned_abs(); // 0 → local (to-bus)
                    let v_target = (vma1 + vmi1) * 0.5;
                    let v_band = (vma1 - vmi1).abs().max(0.001); // at least 0.1% band
                    // ntp1 steps over the full [rmi1, rma1] range
                    let tap_range = (rma1 - rmi1).abs();
                    let tap_step = if ntp1 > 0 {
                        tap_range / ntp1 as f64
                    } else {
                        tap_range / 32.0 // sensible default
                    };
                    if tap_step > 1e-9 {
                        oltc_specs.push(OltcSpec {
                            from_bus,
                            to_bus,
                            circuit: circuit.to_string(),
                            regulated_bus,
                            v_target,
                            v_band,
                            tap_min: rmi1.min(rma1),
                            tap_max: rmi1.max(rma1),
                            tap_step,
                        });
                    }
                }
                3 | -3 => {
                    // Active power flow control (Phase Angle Regulator).
                    // CONT1 is the monitored branch from-bus (negative = to-bus side).
                    // VMA1/VMI1 are target flow bounds in MW (for COD1=3).
                    // RMA1/RMI1 are angle bounds in degrees.
                    let monitored_from_bus = cont1.unsigned_abs();
                    let p_min_mw = vmi1.min(vma1);
                    let p_max_mw = vmi1.max(vma1);
                    let p_target_mw = (p_min_mw + p_max_mw) * 0.5;
                    let p_band_mw = (p_max_mw - p_min_mw).max(1.0); // at least 1 MW band
                    let ang_range = (rma1 - rmi1).abs();
                    let ang_step_deg = if ntp1 > 0 {
                        ang_range / ntp1 as f64
                    } else {
                        ang_range / 32.0
                    };
                    if ang_step_deg > 1e-9 {
                        par_specs.push(ParSpec {
                            from_bus,
                            to_bus,
                            circuit: circuit.to_string(),
                            // Monitored branch from-bus from CONT1 (0 = monitor PAR itself)
                            monitored_from_bus,
                            monitored_to_bus: 0, // not specified in PSS/E 2W record
                            monitored_circuit: "1".to_string(),
                            p_target_mw,
                            p_band_mw,
                            angle_min_deg: rmi1.min(rma1),
                            angle_max_deg: rmi1.max(rma1),
                            ang_step_deg,
                        });
                    }
                }
                _ => {} // COD1 = 0: fixed tap, no outer-loop control
            }

            let tab1: Option<u32> = if rec3_fields.len() > 13 {
                let v = parse_f64(&rec3_fields[13], line_num3, "TAB1").unwrap_or(0.0) as i32;
                if v > 0 { Some(v as u32) } else { None }
            } else {
                None
            };

            // Record 4: WINDV2, NOMV2, ANG2, ...
            pos += 1;
            if pos >= lines.len() {
                return Err(PsseError::UnexpectedEof("Transformer Record 4".into()));
            }
            // PE-02: truncated 2W record — section end encountered before Record 4.
            {
                let peek = lines[pos].trim();
                if is_section_end(peek) || peek.starts_with('Q') || peek.eq_ignore_ascii_case("q") {
                    tracing::warn!(
                        line = pos + 1,
                        from_bus,
                        to_bus,
                        "Truncated 2W transformer Record 4 at line {}; skipping transformer.",
                        pos + 1
                    );
                    return Err(PsseError::UnexpectedEof("Transformer Record 4".into()));
                }
            }
            let rec4_fields = tokenize_record(lines[pos].trim());
            let line_num4 = pos + 1;

            let windv2 = if !rec4_fields.is_empty() {
                parse_f64(&rec4_fields[0], line_num4, "WINDV2")?
            } else {
                1.0
            };

            // Compute 2-winding tap ratio based on CW code
            let tap = match cw {
                1 => {
                    // WINDV in p.u. of bus base kV — tap = windv1/windv2
                    if windv2 != 0.0 {
                        windv1 / windv2
                    } else {
                        windv1
                    }
                }
                2 => {
                    // WINDV in kV — normalise by bus base kV
                    let bkv1 = bus_basekv.get(&from_bus).copied().unwrap_or(1.0);
                    let bkv2 = bus_basekv.get(&to_bus).copied().unwrap_or(1.0);
                    let t1 = if bkv1 > 0.0 { windv1 / bkv1 } else { windv1 };
                    let t2 = if bkv2 > 0.0 { windv2 / bkv2 } else { windv2 };
                    if t2 != 0.0 { t1 / t2 } else { t1 }
                }
                3 => {
                    // WINDV in p.u. of NOMV; NOMV in kV (0 → use bus base kV)
                    let bkv1 = bus_basekv.get(&from_bus).copied().unwrap_or(1.0);
                    let bkv2 = bus_basekv.get(&to_bus).copied().unwrap_or(1.0);
                    let n1 = if nomv1 > 0.0 { nomv1 } else { bkv1 };
                    let nomv2 = if rec4_fields.len() > 1 {
                        parse_f64(&rec4_fields[1], line_num4, "NOMV2").unwrap_or(0.0)
                    } else {
                        0.0
                    };
                    let n2 = if nomv2 > 0.0 { nomv2 } else { bkv2 };
                    let t1 = if bkv1 > 0.0 {
                        windv1 * n1 / bkv1
                    } else {
                        windv1
                    };
                    let t2 = if bkv2 > 0.0 {
                        windv2 * n2 / bkv2
                    } else {
                        windv2
                    };
                    if t2 != 0.0 { t1 / t2 } else { t1 }
                }
                _ => windv1,
            };

            // shift stored in degrees (MATPOWER convention)
            let mut xfmr = make_xfmr_branch(
                from_bus,
                to_bus,
                circuit,
                r12,
                x12,
                rata1,
                ratb1,
                ratc1,
                tap,
                ang1,
                stat > 0,
                mag1,
                mag2,
            );
            xfmr.tab = tab1;
            xfmr.owners = xfmr_owners.clone();
            // Populate discrete step sizes on the Branch from Record 3 tap range data.
            // COD1=1,2 (OLTC): tap_step from NTP1 over [RMI1, RMA1] ratio range.
            // COD1=3 (PAR): phase_step_rad from NTP1 over [RMI1, RMA1] angle range.
            match cod1.abs() {
                1 | 2 => {
                    let tap_range = (rma1 - rmi1).abs();
                    let ts = if ntp1 > 0 {
                        tap_range / ntp1 as f64
                    } else {
                        tap_range / 32.0
                    };
                    if ts > 1e-9 {
                        let ctrl = xfmr
                            .opf_control
                            .get_or_insert_with(BranchOpfControl::default);
                        ctrl.tap_step = ts;
                        ctrl.tap_min = rmi1.min(rma1);
                        ctrl.tap_max = rmi1.max(rma1);
                    }
                }
                3 => {
                    let ang_range = (rma1 - rmi1).abs();
                    let as_deg = if ntp1 > 0 {
                        ang_range / ntp1 as f64
                    } else {
                        ang_range / 32.0
                    };
                    if as_deg > 1e-9 {
                        let ctrl = xfmr
                            .opf_control
                            .get_or_insert_with(BranchOpfControl::default);
                        ctrl.phase_step_rad = as_deg.to_radians();
                        ctrl.phase_min_rad = rmi1.min(rma1).to_radians();
                        ctrl.phase_max_rad = rmi1.max(rma1).to_radians();
                    }
                }
                _ => {}
            }
            transformers.push(xfmr);
        }

        pos += 1;
    }

    Err(PsseError::UnexpectedEof("Transformer Data".into()))
}

// ---------------------------------------------------------------------------
// Area Interchange Data (Section 7)
// ---------------------------------------------------------------------------

/// Parse PSS/E Area Interchange Data section.
///
/// Record format: `ARNUM, ISW, PDES, PTOL, 'ARNAME'`
///
/// Returns `(areas, next_pos)`.
fn parse_area_schedule_section(
    lines: &[&str],
    start: usize,
) -> Result<(Vec<AreaSchedule>, usize), PsseError> {
    let mut areas = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();

        if is_section_end(line) {
            return Ok((areas, pos + 1));
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return Ok((areas, pos + 1));
        }
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }

        let fields = tokenize_record(line);
        if fields.is_empty() {
            pos += 1;
            continue;
        }
        if fields.len() < 5 {
            return Err(PsseError::Parse {
                line: pos + 1,
                message: "area interchange record is truncated".into(),
            });
        }

        let line_num = pos + 1;

        let number = parse_f64(&fields[0], line_num, "ARNUM")? as u32;
        let slack_bus = parse_f64(&fields[1], line_num, "ISW")? as u32;
        let p_desired_mw = parse_f64(&fields[2], line_num, "PDES")?;
        let p_tolerance_mw = parse_f64(&fields[3], line_num, "PTOL")?;
        let name = unquote(&fields[4]);

        areas.push(AreaSchedule {
            number,
            slack_bus,
            p_desired_mw,
            p_tolerance_mw,
            name,
        });

        pos += 1;
    }

    Ok((areas, pos))
}

// ---------------------------------------------------------------------------
// Two-Terminal DC Line Data (Section 8)
// ---------------------------------------------------------------------------

/// Parse PSS/E Two-Terminal DC Line Data section.
///
/// Each DC line has 3 records:
///   Record 1: `NAME, MDC, RDC, SETVL, VSCHD, VCMOD, RCOMP, DELTI, METER, DCVMIN, CCCITMX, CCCACC`
///   Record 2 (rectifier): `IPR, NBR, ALFMX, ALFMN, RCR, XCR, EBASR, TRR, TAPR, TMXR, TMNR, STPR, ICR, IFR, ITR, IDR, XCAPR`
///   Record 3 (inverter):  `IPI, NBI, GAMMX, GAMMN, RCI, XCI, EBASI, TRI, TAPI, TMXI, TMNI, STPI, ICI, IFI, ITI, IDI, XCAPI`
///
/// Returns `(lcc_links, next_pos)`.
fn parse_dc_line_section(
    lines: &[&str],
    start: usize,
) -> Result<(Vec<LccHvdcLink>, usize), PsseError> {
    let mut lcc_links = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        // --- Record 1 ---
        let line1 = lines[pos].trim();

        if is_section_end(line1) {
            return Ok((lcc_links, pos + 1));
        }
        if line1.starts_with('Q') || line1.eq_ignore_ascii_case("q") {
            return Ok((lcc_links, pos + 1));
        }
        if line1.is_empty() || line1.starts_with("@!") {
            pos += 1;
            continue;
        }

        let f1 = tokenize_record(line1);
        if f1.is_empty() {
            pos += 1;
            continue;
        }
        if f1.len() < 12 {
            return Err(PsseError::Parse {
                line: pos + 1,
                message: format!(
                    "truncated PSS/E two-terminal DC record: expected at least 12 fields, got {}",
                    f1.len()
                ),
            });
        }

        let line_num = pos + 1;

        let name = unquote(&f1[0]);
        let mdc = parse_required_f64(&f1[1], line_num, "MDC")? as u32;
        let resistance_ohm = parse_required_f64(&f1[2], line_num, "RDC")?;
        let setvl = parse_required_f64(&f1[3], line_num, "SETVL")?;
        let vschd = parse_required_f64(&f1[4], line_num, "VSCHD")?;
        let vcmod = if f1.len() > 5 {
            parse_f64(&f1[5], line_num, "VCMOD").unwrap_or(0.0)
        } else {
            0.0
        };
        let rcomp = if f1.len() > 6 {
            parse_f64(&f1[6], line_num, "RCOMP").unwrap_or(0.0)
        } else {
            0.0
        };
        let delti = if f1.len() > 7 {
            parse_f64(&f1[7], line_num, "DELTI").unwrap_or(0.0)
        } else {
            0.0
        };
        let meter = if f1.len() > 8 {
            unquote(&f1[8]).chars().next().unwrap_or('I')
        } else {
            'I'
        };
        let dcvmin = if f1.len() > 9 {
            parse_f64(&f1[9], line_num, "DCVMIN").unwrap_or(0.0)
        } else {
            0.0
        };
        let cccitmx = if f1.len() > 10 {
            parse_f64(&f1[10], line_num, "CCCITMX").unwrap_or(20.0) as u32
        } else {
            20
        };
        let cccacc = if f1.len() > 11 {
            parse_f64(&f1[11], line_num, "CCCACC").unwrap_or(1.0)
        } else {
            1.0
        };

        pos += 1;

        // --- Record 2 (rectifier) ---
        if pos >= lines.len() {
            break;
        }
        // Skip @! annotations between records
        while pos < lines.len()
            && (lines[pos].trim().is_empty() || lines[pos].trim().starts_with("@!"))
        {
            pos += 1;
        }
        if pos >= lines.len() {
            break;
        }

        let f2 = tokenize_record(lines[pos].trim());
        let line_num2 = pos + 1;
        let rectifier = parse_dc_converter_record(&f2, line_num2)?;
        pos += 1;

        // --- Record 3 (inverter) ---
        if pos >= lines.len() {
            break;
        }
        while pos < lines.len()
            && (lines[pos].trim().is_empty() || lines[pos].trim().starts_with("@!"))
        {
            pos += 1;
        }
        if pos >= lines.len() {
            break;
        }

        let f3 = tokenize_record(lines[pos].trim());
        let line_num3 = pos + 1;
        let inverter = parse_dc_converter_record(&f3, line_num3)?;
        pos += 1;

        lcc_links.push(LccHvdcLink {
            name,
            mode: LccHvdcControlMode::from_u32(mdc),
            resistance_ohm,
            scheduled_setpoint: setvl,
            scheduled_voltage_kv: vschd,
            voltage_mode_switch_kv: vcmod,
            compounding_resistance_ohm: rcomp,
            current_margin_ka: delti,
            meter,
            voltage_min_kv: dcvmin,
            ac_dc_iteration_max: cccitmx,
            ac_dc_iteration_acceleration: cccacc,
            rectifier,
            inverter,
            // PSS/E raw data doesn't carry a variable-P range; leave at 0/0
            // so the link is treated as fixed at `scheduled_setpoint` by the
            // joint AC-DC OPF (caller can set a range later if wanted).
            p_dc_min_mw: 0.0,
            p_dc_max_mw: 0.0,
        });
    }

    Ok((lcc_links, pos))
}

/// Parse a PSS/E DC converter record (rectifier or inverter).
///
/// Fields: `IBUS, NBRIDGES, ANGMAX, ANGMIN, RC, XC, EBASE, TR, TAP, TAPMAX, TAPMIN, TAPSTEP, IC, IF, IT, ID, XCAP`
fn parse_dc_converter_record(
    fields: &[String],
    line_num: usize,
) -> Result<LccConverterTerminal, PsseError> {
    if fields.len() < 13 {
        return Err(PsseError::Parse {
            line: line_num,
            message: format!(
                "truncated PSS/E DC converter record: expected at least 13 fields, got {}",
                fields.len()
            ),
        });
    }
    let bus = parse_required_f64(&fields[0], line_num, "IBUS")? as u32;
    let n_bridges = parse_required_f64(&fields[1], line_num, "NBRIDGES")? as u32;
    let alpha_max = parse_required_f64(&fields[2], line_num, "ANGMAX")?;
    let alpha_min = parse_required_f64(&fields[3], line_num, "ANGMIN")?;
    let r_comm = parse_required_f64(&fields[4], line_num, "RC")?;
    let x_comm = parse_required_f64(&fields[5], line_num, "XC")?;
    let e_base = parse_required_f64(&fields[6], line_num, "EBASE")?;
    let tr = parse_required_f64(&fields[7], line_num, "TR")?;
    let tap = parse_required_f64(&fields[8], line_num, "TAP")?;
    let tap_max = parse_required_f64(&fields[9], line_num, "TAPMAX")?;
    let tap_min = parse_required_f64(&fields[10], line_num, "TAPMIN")?;
    let tap_step = parse_required_f64(&fields[11], line_num, "TAPSTEP")?;
    let in_service = parse_status(&fields[12], line_num, "IC")? != 0;

    Ok(LccConverterTerminal {
        bus,
        n_bridges,
        alpha_max,
        alpha_min,
        commutation_resistance_ohm: r_comm,
        commutation_reactance_ohm: x_comm,
        base_voltage_kv: e_base,
        turns_ratio: tr,
        tap,
        tap_max,
        tap_min,
        tap_step,
        in_service,
    })
}

// ---------------------------------------------------------------------------
// VSC DC Line Data (Section 9)
// ---------------------------------------------------------------------------

/// Parse PSS/E VSC DC Line Data section.
///
/// Each VSC link has 3 records:
///   Record 1: `'NAME', MDC, RDC, O1, F1, O2, F2`
///   Record 2 (converter 1): `IBUS, TYPE, MODE, DCSET, ACSET, ALOSS, BLOSS, MINLOSS, SMX, SMN, GMX, GMN, BAVAIL, STATE, RMPCT, NREG, VSREG, NREG, VSREF`
///   Record 3 (converter 2): same format as Record 2
///
/// Returns `(vsc_lines, next_pos)`.
fn parse_vsc_dc_section(
    lines: &[&str],
    start: usize,
) -> Result<(Vec<VscHvdcLink>, usize), PsseError> {
    let mut vsc_lines = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        // --- Record 1 ---
        let line1 = lines[pos].trim();

        if is_section_end(line1) {
            return Ok((vsc_lines, pos + 1));
        }
        if line1.starts_with('Q') || line1.eq_ignore_ascii_case("q") {
            return Ok((vsc_lines, pos + 1));
        }
        if line1.is_empty() || line1.starts_with("@!") {
            pos += 1;
            continue;
        }

        let f1 = tokenize_record(line1);
        if f1.is_empty() {
            pos += 1;
            continue;
        }
        if f1.len() < 3 {
            return Err(PsseError::Parse {
                line: pos + 1,
                message: format!(
                    "truncated PSS/E VSC DC record: expected at least 3 fields, got {}",
                    f1.len()
                ),
            });
        }

        let line_num = pos + 1;
        let name = unquote(&f1[0]);
        let mdc = parse_required_f64(&f1[1], line_num, "MDC")? as u32;
        let resistance_ohm = parse_required_f64(&f1[2], line_num, "RDC")?;
        pos += 1;

        // --- Record 2 (converter 1) ---
        while pos < lines.len()
            && (lines[pos].trim().is_empty() || lines[pos].trim().starts_with("@!"))
        {
            pos += 1;
        }
        if pos >= lines.len() {
            break;
        }

        let f2 = tokenize_record(lines[pos].trim());
        let conv1 = parse_vsc_converter_record(&f2, pos + 1)?;
        pos += 1;

        // --- Record 3 (converter 2) ---
        while pos < lines.len()
            && (lines[pos].trim().is_empty() || lines[pos].trim().starts_with("@!"))
        {
            pos += 1;
        }
        if pos >= lines.len() {
            break;
        }

        let f3 = tokenize_record(lines[pos].trim());
        let conv2 = parse_vsc_converter_record(&f3, pos + 1)?;
        pos += 1;

        vsc_lines.push(VscHvdcLink {
            name,
            mode: VscHvdcControlMode::from_u32(mdc),
            resistance_ohm,
            converter1: conv1,
            converter2: conv2,
        });
    }

    Ok((vsc_lines, pos))
}

/// Parse a PSS/E VSC converter record.
///
/// Fields: `IBUS, TYPE, MODE, DCSET, ACSET, ALOSS, BLOSS, MINLOSS, SMX, SMN, GMX, GMN, BAVAIL, STATE, ...`
fn parse_vsc_converter_record(
    fields: &[String],
    line_num: usize,
) -> Result<VscConverterTerminal, PsseError> {
    if fields.len() < 14 {
        return Err(PsseError::Parse {
            line: line_num,
            message: format!(
                "truncated PSS/E VSC converter record: expected at least 14 fields, got {}",
                fields.len()
            ),
        });
    }
    let bus = parse_required_f64(&fields[0], line_num, "IBUS")? as u32;
    // fields[1] = TYPE (1-phase or 3-phase, not relevant here)
    let control_mode = if fields.len() > 2 {
        let m = parse_required_f64(&fields[2], line_num, "MODE")? as u32;
        VscConverterAcControlMode::from_u32(m)
    } else {
        VscConverterAcControlMode::ReactivePower
    };
    let dc_setpoint = if fields.len() > 3 {
        parse_required_f64(&fields[3], line_num, "DCSET")?
    } else {
        return Err(PsseError::Parse {
            line: line_num,
            message: "missing required DCSET field".into(),
        });
    };
    let ac_setpoint = if fields.len() > 4 {
        parse_required_f64(&fields[4], line_num, "ACSET")?
    } else {
        return Err(PsseError::Parse {
            line: line_num,
            message: "missing required ACSET field".into(),
        });
    };
    let loss_a = if fields.len() > 5 {
        parse_required_f64(&fields[5], line_num, "ALOSS")?
    } else {
        return Err(PsseError::Parse {
            line: line_num,
            message: "missing required ALOSS field".into(),
        });
    };
    let loss_b = if fields.len() > 6 {
        parse_required_f64(&fields[6], line_num, "BLOSS")?
    } else {
        return Err(PsseError::Parse {
            line: line_num,
            message: "missing required BLOSS field".into(),
        });
    };
    // fields[7] = MINLOSS (minimum loss — informational only)
    let q_max = if fields.len() > 8 {
        parse_f64(&fields[8], line_num, "SMX").unwrap_or(9999.0)
    } else {
        9999.0
    };
    let q_min = if fields.len() > 9 {
        parse_f64(&fields[9], line_num, "SMN").unwrap_or(-9999.0)
    } else {
        -9999.0
    };
    let v_max = if fields.len() > 10 {
        parse_f64(&fields[10], line_num, "GMX").unwrap_or(1.1)
    } else {
        1.1
    };
    let v_min = if fields.len() > 11 {
        parse_required_f64(&fields[11], line_num, "GMN")?
    } else {
        return Err(PsseError::Parse {
            line: line_num,
            message: "missing required GMN field".into(),
        });
    };
    // fields[12] = BAVAIL, fields[13] = STATE (1 = in-service)
    let in_service = if fields.len() > 13 {
        parse_status(&fields[13], line_num, "STATE")? != 0
    } else {
        return Err(PsseError::Parse {
            line: line_num,
            message: "missing required STATE field".into(),
        });
    };

    Ok(VscConverterTerminal {
        bus,
        control_mode,
        dc_setpoint,
        ac_setpoint,
        loss_constant_mw: loss_a,
        loss_linear: loss_b,
        q_min_mvar: q_min,
        q_max_mvar: q_max,
        voltage_min_pu: v_min,
        voltage_max_pu: v_max,
        in_service,
    })
}

// ---------------------------------------------------------------------------
// FACTS Device Data (Section 14)
// ---------------------------------------------------------------------------

/// Parse PSS/E FACTS Device Data section.
///
/// Record format (one record per device):
///   `'NAME', I, J, MODE, PDES, QDES, VSET, SHMX, TRMX, VTMN, VTMX, VSMX, IMX, LINX, RMPCT, OWNER, SET1, SET2, VSREF, REMOT, MNAME`
///
/// Returns `(facts_devices, next_pos)`.
fn parse_facts_section(lines: &[&str], start: usize) -> (Vec<FactsDevice>, usize) {
    let mut devices = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();

        if is_section_end(line) {
            return (devices, pos + 1);
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return (devices, pos + 1);
        }
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }

        let fields = tokenize_record(line);
        if fields.is_empty() {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;

        let name = unquote(&fields[0]);
        let bus_i = if fields.len() > 1 {
            parse_f64(&fields[1], line_num, "I").unwrap_or(0.0) as u32
        } else {
            0
        };
        let bus_j = if fields.len() > 2 {
            parse_f64(&fields[2], line_num, "J").unwrap_or(0.0) as u32
        } else {
            0
        };
        let mode_val = if fields.len() > 3 {
            parse_f64(&fields[3], line_num, "MODE").unwrap_or(0.0) as u32
        } else {
            0
        };
        let p_des = if fields.len() > 4 {
            parse_f64(&fields[4], line_num, "PDES").unwrap_or(0.0)
        } else {
            0.0
        };
        let q_des = if fields.len() > 5 {
            parse_f64(&fields[5], line_num, "QDES").unwrap_or(0.0)
        } else {
            0.0
        };
        let v_set = if fields.len() > 6 {
            parse_f64(&fields[6], line_num, "VSET").unwrap_or(1.0)
        } else {
            1.0
        };
        let q_max = if fields.len() > 7 {
            parse_f64(&fields[7], line_num, "SHMX").unwrap_or(9999.0)
        } else {
            9999.0
        };
        // fields[8] = TRMX (max transformer turns ratio — informational)
        // fields[9] = VTMN, fields[10] = VTMX (voltage limits at controlled bus)
        // fields[11] = VSMX (max series voltage — informational)
        // fields[12] = IMX (max current — informational)
        let linx = if fields.len() > 13 {
            parse_f64(&fields[13], line_num, "LINX").unwrap_or(0.0)
        } else {
            0.0
        };

        let mode = FactsMode::from_u32(mode_val);
        let in_service = mode.in_service();

        // Infer FACTS device type from operating mode:
        //   ShuntOnly → SVC (shunt reactive compensation)
        //   SeriesOnly / ImpedanceModulation → TCSC (series impedance control)
        //   ShuntSeries / SeriesPowerControl → UPFC (combined shunt + series)
        let facts_type = match mode {
            FactsMode::ShuntOnly => FactsType::Svc,
            FactsMode::SeriesOnly | FactsMode::ImpedanceModulation => FactsType::Tcsc,
            FactsMode::ShuntSeries | FactsMode::SeriesPowerControl => FactsType::Upfc,
            FactsMode::OutOfService => FactsType::Svc, // default for OOS devices
        };

        devices.push(FactsDevice {
            name,
            bus_from: bus_i,
            bus_to: bus_j,
            mode,
            p_setpoint_mw: p_des,
            q_setpoint_mvar: q_des,
            voltage_setpoint_pu: v_set,
            q_max,
            series_reactance_pu: linx,
            in_service,
            facts_type,
            ..FactsDevice::default()
        });

        pos += 1;
    }

    (devices, pos)
}

// ---------------------------------------------------------------------------
// Tokenizer — handles comma and/or whitespace-delimited records with quoted strings
// ---------------------------------------------------------------------------

fn tokenize_record(line: &str) -> Vec<String> {
    let line = line.trim();
    // Strip trailing comment (everything after / that's not inside quotes)
    let line = strip_comment(line);
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }

    if line.contains(',') {
        // Comma-delimited: split by commas, respecting quoted strings
        let mut tokens = Vec::new();
        let mut current = String::new();
        let mut in_quotes = false;

        for ch in line.chars() {
            match ch {
                '\'' => {
                    in_quotes = !in_quotes;
                    current.push(ch);
                }
                ',' if !in_quotes => {
                    tokens.push(current.trim().to_string());
                    current.clear();
                }
                _ => current.push(ch),
            }
        }
        // Push last field
        let last = current.trim().to_string();
        if !last.is_empty() {
            tokens.push(last);
        }
        tokens
    } else {
        // Whitespace-delimited (no commas in record)
        let mut tokens = Vec::new();
        let mut chars = line.chars().peekable();

        while let Some(&ch) = chars.peek() {
            if ch == '\'' {
                // Quoted string
                chars.next();
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '\'' {
                        chars.next();
                        break;
                    }
                    s.push(c);
                    chars.next();
                }
                tokens.push(format!("'{s}'"));
            } else if ch == ' ' || ch == '\t' {
                chars.next();
            } else {
                let mut tok = String::new();
                while let Some(&c) = chars.peek() {
                    if c == ' ' || c == '\t' {
                        break;
                    }
                    tok.push(c);
                    chars.next();
                }
                tokens.push(tok);
            }
        }
        tokens
    }
}

/// Strip the trailing comment from a PSS/E record line.
/// Comments start with `/` but we must not strip `/` inside quoted strings.
fn strip_comment(line: &str) -> &str {
    let mut in_quotes = false;
    for (i, ch) in line.char_indices() {
        match ch {
            '\'' => in_quotes = !in_quotes,
            '/' if !in_quotes => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Check if a line is a PSS/E section terminator.
///
/// Fix PE-01: the only valid terminators are the bare `"0"` token or a line
/// starting with `"0 /"` (i.e. `"0 / END OF …"`).  The old code also matched
/// `"0,"` and `"0\t"` / `"0 "` which incorrectly terminated sections when a
/// branch or transformer had bus 0 as its from-bus.
fn is_section_end(line: &str) -> bool {
    let line = line.trim();
    if line == "0" {
        return true;
    }
    if line.starts_with("0 /") || line.starts_with("0\t/") {
        return true;
    }
    false
}

/// Parse up to 4 (Oi, Fi) owner pairs from a PSS/E record.
fn parse_multi_owner_fields(
    fields: &[String],
    start_idx: usize,
) -> Vec<surge_network::network::OwnershipEntry> {
    let mut owners = Vec::new();
    for i in 0..4 {
        let oi_idx = start_idx + i * 2;
        let fi_idx = oi_idx + 1;
        if fields.len() <= oi_idx {
            break;
        }
        let oi = fields[oi_idx]
            .trim()
            .trim_matches('\'')
            .parse::<f64>()
            .unwrap_or(0.0) as u32;
        if oi == 0 {
            break;
        }
        let fi = if fields.len() > fi_idx {
            fields[fi_idx]
                .trim()
                .trim_matches('\'')
                .parse::<f64>()
                .unwrap_or(1.0)
        } else {
            1.0
        };
        owners.push(surge_network::network::OwnershipEntry {
            owner: oi,
            fraction: fi,
        });
    }
    owners
}

/// Parse a token as f64, with helpful error context.
fn parse_f64(token: &str, line: usize, field: &str) -> Result<f64, PsseError> {
    let token = token.trim();
    if token.is_empty() {
        return Ok(0.0); // default for missing fields
    }
    // Remove surrounding quotes
    let token = if token.starts_with('\'') && token.ends_with('\'') {
        &token[1..token.len() - 1]
    } else {
        token
    };
    let token = token.trim();
    if token.is_empty() {
        return Ok(0.0);
    }
    let val = token.parse::<f64>().map_err(|_| PsseError::Parse {
        line,
        message: format!("invalid {field} value: '{token}'"),
    })?;
    // PE-04: reject NaN / Inf — they propagate silently through power-flow arithmetic.
    if !val.is_finite() {
        return Err(PsseError::NonFiniteValue {
            line,
            message: format!("field '{field}' parsed to non-finite value '{token}' (NaN or Inf)"),
        });
    }
    Ok(val)
}

fn parse_required_f64(token: &str, line: usize, field: &str) -> Result<f64, PsseError> {
    let token = token.trim();
    if token.is_empty() {
        return Err(PsseError::Parse {
            line,
            message: format!("missing required {field} value"),
        });
    }
    parse_f64(token, line, field)
}

/// Parse a required PSS/E STATUS field that may be numeric (1/0) or textual ('A'/'I'/'BL').
///
/// Older PSS/E versions (v29/v30) use text codes:
///   'A' (active) or 'I' (in-service) → 1
///   'BL' (blank/disconnected), 'O' (out-of-service), '0' → 0
///
/// Missing or unknown values are rejected so we do not fail open to in-service.
fn parse_status(token: &str, line: usize, field: &str) -> Result<i32, PsseError> {
    let tok = token.trim().trim_matches('\'').trim_matches('"').trim();
    if tok.is_empty() {
        return Err(PsseError::Parse {
            line,
            message: format!("missing required {field} value"),
        });
    }
    // Try numeric first
    if let Ok(v) = tok.parse::<f64>() {
        return Ok(v as i32);
    }
    // Text status codes (case-insensitive)
    match tok.to_uppercase().as_str() {
        "A" | "I" => Ok(1),  // Active / In-service
        "BL" | "O" => Ok(0), // Blank / Out-of-service
        _ => Err(PsseError::Parse {
            line,
            message: format!("unknown {field} status code: '{tok}'"),
        }),
    }
}

// ---------------------------------------------------------------------------
// Zone Data (Section 13)
// ---------------------------------------------------------------------------

/// Parse PSS/E Zone Data section.
///
/// Record format: `ZONUM, 'ZONAME'`
///
/// Returns `(zones, next_pos)`.
fn parse_zone_section(lines: &[&str], start: usize) -> (Vec<Region>, usize) {
    let mut zones = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();

        if is_section_end(line) {
            return (zones, pos + 1);
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return (zones, pos + 1);
        }
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }

        let fields = tokenize_record(line);
        if fields.is_empty() {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;

        let number = match parse_f64(&fields[0], line_num, "ZONUM") {
            Ok(v) => v as u32,
            Err(_) => {
                pos += 1;
                continue;
            }
        };
        let name = if fields.len() > 1 {
            unquote(&fields[1])
        } else {
            String::new()
        };

        zones.push(Region { number, name });
        pos += 1;
    }

    (zones, pos)
}

// ---------------------------------------------------------------------------
// Owner Data (Section 13c)
// ---------------------------------------------------------------------------

/// Parse PSS/E Owner Data section.
///
/// Record format: `OWNUM, 'OWNAME'`
///
/// Returns `(owners, next_pos)`.
fn parse_owner_section(lines: &[&str], start: usize) -> (Vec<Owner>, usize) {
    let mut owners = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();

        if is_section_end(line) {
            return (owners, pos + 1);
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return (owners, pos + 1);
        }
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }

        let fields = tokenize_record(line);
        if fields.is_empty() {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;

        let number = match parse_f64(&fields[0], line_num, "OWNUM") {
            Ok(v) => v as u32,
            Err(_) => {
                pos += 1;
                continue;
            }
        };
        let name = if fields.len() > 1 {
            unquote(&fields[1])
        } else {
            String::new()
        };

        owners.push(Owner { number, name });
        pos += 1;
    }

    (owners, pos)
}

// ---------------------------------------------------------------------------
// Inter-Area Transfer Data (Section 13b)
// ---------------------------------------------------------------------------

/// Parse PSS/E Inter-Area Transfer Data section.
///
/// Record format: `ARFROM, ARTO, TRID, PTRAN`
///
/// Returns `(transfers, next_pos)`.
fn parse_inter_area_transfer_section(
    lines: &[&str],
    start: usize,
) -> (Vec<ScheduledAreaTransfer>, usize) {
    let mut transfers = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();

        if is_section_end(line) {
            return (transfers, pos + 1);
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return (transfers, pos + 1);
        }
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }

        let fields = tokenize_record(line);
        if fields.len() < 2 {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;

        let from_area = match parse_f64(&fields[0], line_num, "ARFROM") {
            Ok(v) => v as u32,
            Err(_) => {
                pos += 1;
                continue;
            }
        };
        let to_area = match parse_f64(&fields[1], line_num, "ARTO") {
            Ok(v) => v as u32,
            Err(_) => {
                pos += 1;
                continue;
            }
        };
        let id = if fields.len() > 2 {
            parse_f64(&fields[2], line_num, "TRID").unwrap_or(1.0) as u32
        } else {
            1
        };
        let p_transfer_mw = if fields.len() > 3 {
            parse_f64(&fields[3], line_num, "PTRAN").unwrap_or(0.0)
        } else {
            0.0
        };

        transfers.push(ScheduledAreaTransfer {
            from_area,
            to_area,
            id,
            p_transfer_mw,
        });
        pos += 1;
    }

    (transfers, pos)
}

// ---------------------------------------------------------------------------
// Impedance Correction Data (Section 10)
// ---------------------------------------------------------------------------

/// Parse PSS/E Impedance Correction Data section.
///
/// Record format: `I, T1, F1, T2, F2, ..., T11, F11`
///
/// The first field is the table number, then up to 11 (T, F) pairs.
/// Pairs where both T and F are 0.0 are skipped.
///
/// Returns `(tables, next_pos)`.
fn parse_impedance_correction_section(
    lines: &[&str],
    start: usize,
) -> (Vec<ImpedanceCorrectionTable>, usize) {
    let mut tables = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();

        if is_section_end(line) {
            return (tables, pos + 1);
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return (tables, pos + 1);
        }
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }

        let fields = tokenize_record(line);
        if fields.is_empty() {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;

        let number = match parse_f64(&fields[0], line_num, "I") {
            Ok(v) => v as u32,
            Err(_) => {
                pos += 1;
                continue;
            }
        };

        let mut entries = Vec::new();
        // Fields 1..N are T1,F1,T2,F2,... pairs
        let mut i = 1;
        while i + 1 < fields.len() {
            let t = parse_f64(&fields[i], line_num, "T").unwrap_or(0.0);
            let f = parse_f64(&fields[i + 1], line_num, "F").unwrap_or(0.0);
            if t != 0.0 || f != 0.0 {
                entries.push((t, f));
            }
            i += 2;
        }

        tables.push(ImpedanceCorrectionTable { number, entries });
        pos += 1;
    }

    (tables, pos)
}

// ---------------------------------------------------------------------------
// Multi-Section Line Data (Section 12)
// ---------------------------------------------------------------------------

/// Parse PSS/E Multi-Section Line Data section.
///
/// Record format: `I, J, 'ID', MET, DUM1, DUM2, ...`
///
/// Variable-length dummy bus list after the first 4 fields.
///
/// Returns `(groups, next_pos)`.
fn parse_multi_section_line_section(
    lines: &[&str],
    start: usize,
) -> (Vec<MultiSectionLineGroup>, usize) {
    let mut groups = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();

        if is_section_end(line) {
            return (groups, pos + 1);
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return (groups, pos + 1);
        }
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }

        let fields = tokenize_record(line);
        if fields.len() < 3 {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;

        let from_bus = match parse_f64(&fields[0], line_num, "I") {
            Ok(v) => v as u32,
            Err(_) => {
                pos += 1;
                continue;
            }
        };
        let to_bus = match parse_f64(&fields[1], line_num, "J") {
            Ok(v) => v as u32,
            Err(_) => {
                pos += 1;
                continue;
            }
        };
        let id = unquote(&fields[2]);
        let metered_end = if fields.len() > 3 {
            parse_f64(&fields[3], line_num, "MET").unwrap_or(1.0) as u32
        } else {
            1
        };

        // Remaining fields are dummy bus numbers
        let mut dummy_buses = Vec::new();
        for fi in 4..fields.len() {
            let bus = parse_f64(&fields[fi], line_num, "DUM").unwrap_or(0.0) as u32;
            if bus != 0 {
                dummy_buses.push(bus);
            }
        }

        groups.push(MultiSectionLineGroup {
            from_bus,
            to_bus,
            id,
            metered_end,
            dummy_buses,
        });
        pos += 1;
    }

    (groups, pos)
}

// ---------------------------------------------------------------------------
// Multi-Terminal DC Data (Section 11)
// ---------------------------------------------------------------------------

/// Parse PSS/E Multi-Terminal DC Data section.
///
/// Each system starts with a header line, followed by NCONV converter records,
/// NDCBS DC bus records, and NDCLN DC link records. The section ends with `0`.
///
/// Header: `'NAME', NCONV, NDCBS, NDCLN, MDC, VCONV, VCMOD, VCONVN`
/// Converter: `IB, N, ANGMX, ANGMN, RC, XC, EBAS, TR, TAP, TPMX, TPMN, TSTP, SETVL, DCPF, MARG, CNVCOD`
/// DC bus: `IDC, IB, AREA, ZONE, 'DCNAME', IDC2, RGRND, OWNER`
/// DC link: `IDC, JDC, 'DCCKT', MET, RDC, LDC`
///
/// Returns `(mt_lines, next_pos)`.
fn parse_multi_terminal_dc_section(lines: &[&str], start: usize) -> (Vec<RawMtdcSystem>, usize) {
    let mut mt_lines = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();

        if is_section_end(line) {
            return (mt_lines, pos + 1);
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return (mt_lines, pos + 1);
        }
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }

        // Parse header line
        let fields = tokenize_record(line);
        if fields.is_empty() {
            pos += 1;
            continue;
        }

        let line_num = pos + 1;

        let name = unquote(&fields[0]);
        let nconv = if fields.len() > 1 {
            parse_f64(&fields[1], line_num, "NCONV").unwrap_or(0.0) as u32
        } else {
            0
        };
        let ndcbs = if fields.len() > 2 {
            parse_f64(&fields[2], line_num, "NDCBS").unwrap_or(0.0) as u32
        } else {
            0
        };
        let ndcln = if fields.len() > 3 {
            parse_f64(&fields[3], line_num, "NDCLN").unwrap_or(0.0) as u32
        } else {
            0
        };
        let mdc = if fields.len() > 4 {
            parse_f64(&fields[4], line_num, "MDC").unwrap_or(1.0) as u32
        } else {
            1
        };
        let vconv = if fields.len() > 5 {
            parse_f64(&fields[5], line_num, "VCONV").unwrap_or(0.0)
        } else {
            0.0
        };
        let vcmod = if fields.len() > 6 {
            parse_f64(&fields[6], line_num, "VCMOD").unwrap_or(0.0)
        } else {
            0.0
        };
        let vconvn = if fields.len() > 7 {
            parse_f64(&fields[7], line_num, "VCONVN").unwrap_or(0.0)
        } else {
            0.0
        };

        pos += 1;

        // Parse NCONV converter records
        let mut converters = Vec::new();
        while converters.len() < nconv as usize && pos < lines.len() {
            let cline = lines[pos].trim();
            if cline.is_empty() || cline.starts_with("@!") {
                pos += 1;
                continue;
            }
            let cf = tokenize_record(cline);
            let cl = pos + 1;

            let bus = if !cf.is_empty() {
                parse_f64(&cf[0], cl, "IB").unwrap_or(0.0) as u32
            } else {
                0
            };
            let n_bridges = if cf.len() > 1 {
                parse_f64(&cf[1], cl, "N").unwrap_or(1.0) as u32
            } else {
                1
            };
            let alpha_max = if cf.len() > 2 {
                parse_f64(&cf[2], cl, "ANGMX").unwrap_or(90.0)
            } else {
                90.0
            };
            let alpha_min = if cf.len() > 3 {
                parse_f64(&cf[3], cl, "ANGMN").unwrap_or(5.0)
            } else {
                5.0
            };
            let r_comm = if cf.len() > 4 {
                parse_f64(&cf[4], cl, "RC").unwrap_or(0.0)
            } else {
                0.0
            };
            let x_comm = if cf.len() > 5 {
                parse_f64(&cf[5], cl, "XC").unwrap_or(0.0)
            } else {
                0.0
            };
            let e_base = if cf.len() > 6 {
                parse_f64(&cf[6], cl, "EBAS").unwrap_or(0.0)
            } else {
                0.0
            };
            let tr = if cf.len() > 7 {
                parse_f64(&cf[7], cl, "TR").unwrap_or(1.0)
            } else {
                1.0
            };
            let tap = if cf.len() > 8 {
                parse_f64(&cf[8], cl, "TAP").unwrap_or(1.0)
            } else {
                1.0
            };
            let tap_max = if cf.len() > 9 {
                parse_f64(&cf[9], cl, "TPMX").unwrap_or(1.1)
            } else {
                1.1
            };
            let tap_min = if cf.len() > 10 {
                parse_f64(&cf[10], cl, "TPMN").unwrap_or(0.9)
            } else {
                0.9
            };
            let tap_step = if cf.len() > 11 {
                parse_f64(&cf[11], cl, "TSTP").unwrap_or(0.00625)
            } else {
                0.00625
            };
            let setvl = if cf.len() > 12 {
                parse_f64(&cf[12], cl, "SETVL").unwrap_or(0.0)
            } else {
                0.0
            };
            let dcpf = if cf.len() > 13 {
                parse_f64(&cf[13], cl, "DCPF").unwrap_or(0.0)
            } else {
                0.0
            };
            let marg = if cf.len() > 14 {
                parse_f64(&cf[14], cl, "MARG").unwrap_or(0.0)
            } else {
                0.0
            };
            let cnvcod = if cf.len() > 15 {
                parse_f64(&cf[15], cl, "CNVCOD").unwrap_or(1.0) as u32
            } else {
                1
            };

            converters.push(RawMtdcConverter {
                bus,
                n_bridges,
                alpha_max,
                alpha_min,
                commutation_resistance_ohm: r_comm,
                commutation_reactance_ohm: x_comm,
                base_voltage_kv: e_base,
                turns_ratio: tr,
                tap,
                tap_max,
                tap_min,
                tap_step,
                scheduled_setpoint: setvl,
                dcpf,
                marg,
                cnvcod,
            });
            pos += 1;
        }

        // Parse NDCBS DC bus records
        let mut dc_buses = Vec::new();
        while dc_buses.len() < ndcbs as usize && pos < lines.len() {
            let bline = lines[pos].trim();
            if bline.is_empty() || bline.starts_with("@!") {
                pos += 1;
                continue;
            }
            let bf = tokenize_record(bline);
            let bl = pos + 1;

            let dc_bus = if !bf.is_empty() {
                parse_f64(&bf[0], bl, "IDC").unwrap_or(0.0) as u32
            } else {
                0
            };
            let ac_bus = if bf.len() > 1 {
                parse_f64(&bf[1], bl, "IB").unwrap_or(0.0) as u32
            } else {
                0
            };
            let area = if bf.len() > 2 {
                parse_f64(&bf[2], bl, "AREA").unwrap_or(1.0) as u32
            } else {
                1
            };
            let zone = if bf.len() > 3 {
                parse_f64(&bf[3], bl, "ZONE").unwrap_or(1.0) as u32
            } else {
                1
            };
            let dc_name = if bf.len() > 4 {
                unquote(&bf[4])
            } else {
                String::new()
            };
            let idc2 = if bf.len() > 5 {
                parse_f64(&bf[5], bl, "IDC2").unwrap_or(0.0) as u32
            } else {
                0
            };
            let rgrnd = if bf.len() > 6 {
                parse_f64(&bf[6], bl, "RGRND").unwrap_or(0.0)
            } else {
                0.0
            };
            let owner = if bf.len() > 7 {
                parse_f64(&bf[7], bl, "OWNER").unwrap_or(1.0) as u32
            } else {
                1
            };

            dc_buses.push(RawMtdcBus {
                dc_bus,
                ac_bus,
                area,
                zone,
                name: dc_name,
                idc2,
                rgrnd,
                owner,
            });
            pos += 1;
        }

        // Parse NDCLN DC link records
        let mut dc_links = Vec::new();
        while dc_links.len() < ndcln as usize && pos < lines.len() {
            let lline = lines[pos].trim();
            if lline.is_empty() || lline.starts_with("@!") {
                pos += 1;
                continue;
            }
            let lf = tokenize_record(lline);
            let ll = pos + 1;

            let from_dc_bus = if !lf.is_empty() {
                parse_f64(&lf[0], ll, "IDC").unwrap_or(0.0) as u32
            } else {
                0
            };
            let to_dc_bus = if lf.len() > 1 {
                parse_f64(&lf[1], ll, "JDC").unwrap_or(0.0) as u32
            } else {
                0
            };
            let circuit = if lf.len() > 2 {
                unquote(&lf[2])
            } else {
                "1".to_string()
            };
            let metered = if lf.len() > 3 {
                parse_f64(&lf[3], ll, "MET").unwrap_or(1.0) as u32
            } else {
                1
            };
            let resistance_ohm = if lf.len() > 4 {
                parse_f64(&lf[4], ll, "RDC").unwrap_or(0.0)
            } else {
                0.0
            };
            let ldc = if lf.len() > 5 {
                parse_f64(&lf[5], ll, "LDC").unwrap_or(0.0)
            } else {
                0.0
            };

            dc_links.push(RawMtdcLink {
                from_dc_bus,
                to_dc_bus,
                circuit,
                metered,
                resistance_ohm,
                ldc,
            });
            pos += 1;
        }

        mt_lines.push(RawMtdcSystem {
            name,
            n_converters: nconv,
            n_dc_buses: ndcbs,
            n_dc_links: ndcln,
            control_mode: mdc,
            dc_voltage_kv: vconv,
            voltage_mode_switch_kv: vcmod,
            dc_voltage_min_kv: vconvn,
            converters,
            dc_buses,
            dc_links,
        });
    }

    (mt_lines, pos)
}

fn normalize_dc_grids(network: &mut Network, systems: &[RawMtdcSystem]) {
    let mut next_grid = network.hvdc.next_dc_grid_id();
    let mut next_dc_bus_id = network.hvdc.next_dc_bus_id();

    for system in systems {
        if system.control_mode == 0 {
            continue;
        }

        let grid_id = next_grid;
        next_grid += 1;

        let base_kv_dc = if system.dc_voltage_kv > 0.0 {
            system.dc_voltage_kv
        } else {
            system
                .converters
                .iter()
                .find(|converter| converter.base_voltage_kv > 0.0)
                .map(|converter| converter.base_voltage_kv)
                .unwrap_or(500.0)
        };

        let mut local_to_global: HashMap<u32, u32> = HashMap::new();
        for dc_bus in &system.dc_buses {
            let global_id = next_dc_bus_id;
            next_dc_bus_id += 1;
            local_to_global.insert(dc_bus.dc_bus, global_id);
            network
                .hvdc
                .ensure_dc_grid(grid_id, Some(system.name.clone()))
                .buses
                .push(DcBus {
                    bus_id: global_id,
                    p_dc_mw: 0.0,
                    v_dc_pu: 1.0,
                    base_kv_dc,
                    v_dc_max: 1.1,
                    v_dc_min: 0.9,
                    cost: 0.0,
                    g_shunt_siemens: 0.0,
                    r_ground_ohm: dc_bus.rgrnd,
                });
        }

        for converter in &system.converters {
            let Some(&dc_bus) = system
                .dc_buses
                .iter()
                .find(|dc_bus| dc_bus.ac_bus == converter.bus)
                .and_then(|dc_bus| local_to_global.get(&dc_bus.dc_bus))
            else {
                continue;
            };

            let scheduled_setpoint = if converter.cnvcod == 2 {
                -converter.scheduled_setpoint.abs()
            } else {
                converter.scheduled_setpoint.abs()
            };

            network
                .hvdc
                .ensure_dc_grid(grid_id, Some(system.name.clone()))
                .converters
                .push(DcConverter::Lcc(LccDcConverter {
                    id: String::new(),
                    dc_bus,
                    ac_bus: converter.bus,
                    n_bridges: converter.n_bridges,
                    alpha_max_deg: converter.alpha_max,
                    alpha_min_deg: converter.alpha_min,
                    gamma_min_deg: 15.0,
                    commutation_resistance_ohm: converter.commutation_resistance_ohm,
                    commutation_reactance_ohm: converter.commutation_reactance_ohm,
                    base_voltage_kv: converter.base_voltage_kv.max(base_kv_dc),
                    turns_ratio: converter.turns_ratio,
                    tap_ratio: converter.tap,
                    tap_max: converter.tap_max,
                    tap_min: converter.tap_min,
                    tap_step: converter.tap_step,
                    scheduled_setpoint,
                    power_share_percent: converter.dcpf,
                    current_margin_percent: converter.marg,
                    role: if converter.cnvcod == 2 {
                        LccDcConverterRole::Inverter
                    } else {
                        LccDcConverterRole::Rectifier
                    },
                    in_service: true,
                }));
        }

        for link in &system.dc_links {
            let Some(&from_bus) = local_to_global.get(&link.from_dc_bus) else {
                continue;
            };
            let Some(&to_bus) = local_to_global.get(&link.to_dc_bus) else {
                continue;
            };
            let grid = network
                .hvdc
                .ensure_dc_grid(grid_id, Some(system.name.clone()));
            grid.branches.push(DcBranch {
                id: format!("dc_grid_{}_branch_{}", grid.id, grid.branches.len() + 1),
                from_bus,
                to_bus,
                r_ohm: link.resistance_ohm,
                l_mh: link.ldc,
                c_uf: 0.0,
                rating_a_mva: 0.0,
                rating_b_mva: 0.0,
                rating_c_mva: 0.0,
                status: true,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Skip section helper - consume lines until section terminator
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn skip_section(lines: &[&str], start: usize) -> usize {
    let mut pos = start;
    while pos < lines.len() {
        let line = lines[pos].trim();
        if is_section_end(line) {
            return pos + 1;
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            return pos + 1;
        }
        pos += 1;
    }
    pos
}

// ---------------------------------------------------------------------------
// System Switching Device Data (v34+, between Branch and Transformer sections)
// ---------------------------------------------------------------------------

/// A system-level switching device connecting nodes across different buses.
struct RawSystemSwitchDevice {
    /// "From" bus number.
    bus_i: u32,
    /// "To" bus number.
    bus_j: u32,
    /// Circuit/device identifier.
    ckt: String,
    /// Device name.
    name: String,
    /// Device type: 1=ZBR, 2=Breaker, 3=Disconnect.
    device_type: u32,
    /// Operating status: 1=closed, 0=open.
    status: u32,
    /// Normal status: 1=normally closed, 0=normally open.
    normal_status: u32,
    /// Reactance in per-unit (stored for future use in impedance modeling).
    #[allow(dead_code)]
    x_pu: f64,
    /// MVA rating (first rating set).
    rate1: f64,
}

fn psse_switch_type(device_type: u32) -> SwitchType {
    match device_type {
        1 => SwitchType::Switch, // ZBR (zero bus reactance)
        2 => SwitchType::Breaker,
        3 => SwitchType::Disconnector,
        _ => SwitchType::Switch,
    }
}

// ---------------------------------------------------------------------------
// Voltage Droop Control Data (v36)
// ---------------------------------------------------------------------------

fn parse_voltage_droop_control_section(
    lines: &[&str],
    start: usize,
) -> (
    Vec<surge_network::network::voltage_droop_control::VoltageDroopControl>,
    usize,
) {
    let mut controls = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }
        if is_section_end(line) {
            pos += 1;
            break;
        }

        let fields = tokenize_record(line);
        if fields.len() < 7 {
            pos += 1;
            continue;
        }

        controls.push(
            surge_network::network::voltage_droop_control::VoltageDroopControl {
                bus: fields[0].parse().unwrap_or(0),
                device_id: fields[1].clone(),
                device_type: fields[2].parse().unwrap_or(1),
                regulated_bus: fields[3].parse().unwrap_or(0),
                vdrp: fields[4].parse().unwrap_or(0.0),
                vmax: fields[5].parse().unwrap_or(1.1),
                vmin: fields[6].parse().unwrap_or(0.9),
            },
        );
        pos += 1;
    }

    if !controls.is_empty() {
        tracing::debug!(count = controls.len(), "parsed voltage droop control data");
    }
    (controls, pos)
}

// ---------------------------------------------------------------------------
// Switching Device Rating Set Data (v36)
// ---------------------------------------------------------------------------

fn parse_switching_device_rating_set_section(
    lines: &[&str],
    start: usize,
) -> (
    Vec<surge_network::network::switching_device_rating::SwitchingDeviceRatingSet>,
    usize,
) {
    let mut sets = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }
        if is_section_end(line) {
            pos += 1;
            break;
        }

        let fields = tokenize_record(line);
        if fields.len() < 7 {
            pos += 1;
            continue;
        }

        let mut additional = Vec::new();
        for i in 7..fields.len() {
            if let Ok(r) = fields[i].parse::<f64>() {
                additional.push(r);
            }
        }

        sets.push(
            surge_network::network::switching_device_rating::SwitchingDeviceRatingSet {
                from_bus: fields[0].parse().unwrap_or(0),
                to_bus: fields[1].parse().unwrap_or(0),
                circuit: fields[2].clone(),
                rating_set: fields[3].parse().unwrap_or(1),
                rate1: fields[4].parse().unwrap_or(0.0),
                rate2: fields[5].parse().unwrap_or(0.0),
                rate3: fields[6].parse().unwrap_or(0.0),
                additional_rates: additional,
            },
        );
        pos += 1;
    }

    if !sets.is_empty() {
        tracing::debug!(
            count = sets.len(),
            "parsed switching device rating set data"
        );
    }
    (sets, pos)
}

/// Parse the SYSTEM SWITCHING DEVICE DATA section (v34+).
///
/// This section sits between Branch Data and Transformer Data.  Records have
/// the format: NI, NJ, CKT, NAME, TYPE, STATUS, NSTAT, X, RATE1, RATE2, RATE3
///
/// Returns (devices, next_pos).  If the section is empty or missing, returns
/// an empty vec and the same position.
fn parse_system_switching_device_section(
    lines: &[&str],
    start: usize,
) -> (Vec<RawSystemSwitchDevice>, usize) {
    let mut devices = Vec::new();
    let mut pos = start;

    while pos < lines.len() {
        let line = lines[pos].trim();
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }
        if is_section_end(line) {
            pos += 1;
            break;
        }

        let fields = tokenize_record(line);
        if fields.len() < 6 {
            pos += 1;
            continue;
        }

        let bus_i = fields[0].parse::<u32>().unwrap_or(0);
        let bus_j = fields[1].parse::<u32>().unwrap_or(0);
        let ckt = fields.get(2).cloned().unwrap_or_else(|| "1".into());
        let name = fields.get(3).cloned().unwrap_or_default();
        let device_type = fields.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);
        let status = fields.get(5).and_then(|s| s.parse().ok()).unwrap_or(1);
        let normal_status = fields.get(6).and_then(|s| s.parse().ok()).unwrap_or(1);
        let x_pu = fields.get(7).and_then(|s| s.parse().ok()).unwrap_or(0.0001);
        let rate1 = fields.get(8).and_then(|s| s.parse().ok()).unwrap_or(0.0);

        devices.push(RawSystemSwitchDevice {
            bus_i,
            bus_j,
            ckt,
            name,
            device_type,
            status,
            normal_status,
            x_pu,
            rate1,
        });
        pos += 1;
    }

    if !devices.is_empty() {
        tracing::debug!(count = devices.len(), "parsed system switching device data");
    }
    (devices, pos)
}

// ---------------------------------------------------------------------------
// Substation Data (v35+)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Induction Machine Data (v35+)
// ---------------------------------------------------------------------------

/// Parse the INDUCTION MACHINE DATA section (v35+).
///
/// Each two-line record (continuation with `/`) describes one induction motor:
///
/// Line 1: `I, 'ID', STAT, SCODE, DCODE, AREA, ZONE, OWNER, TCODE, BCODE,
///           MBASE, RATEKV, PCODE, PSET, H, A, B, D, E, F`
/// Line 2: `RA, XA, XM, R1, X1, R2, X2, X3, E1, SE1, E2, SE2, IA1, IA2, XAMULT`
///
/// Tokens beyond what Surge currently uses are silently ignored.
fn parse_induction_machine_section(
    lines: &[&str],
    start: usize,
) -> Vec<surge_network::network::induction_machine::InductionMachine> {
    use surge_network::network::induction_machine::InductionMachine;
    let mut machines = Vec::new();
    // `start` already points to the first data line (seek_section consumed the marker).
    let mut pos = start;

    while pos < lines.len() {
        let l = lines[pos].trim();
        if l.is_empty() || l.starts_with("@!") {
            pos += 1;
            continue;
        }
        if is_section_end(l) {
            break;
        }

        let f1 = tokenize_record(l);
        pos += 1;
        if f1.len() < 4 {
            continue;
        }

        // Line 2 (circuit parameters) — optional, may be absent in malformed input.
        let f2 = if pos < lines.len() {
            let l2 = lines[pos].trim();
            if !l2.is_empty() && !is_section_end(l2) && !l2.starts_with("@!") {
                pos += 1;
                tokenize_record(l2)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let bus: u32 = f1[0].parse().unwrap_or(0);
        let id = f1
            .get(1)
            .map(|s| s.trim().trim_matches('\'').to_string())
            .unwrap_or_else(|| "1".to_string());
        let stat: i32 = f1.get(2).and_then(|t| t.parse().ok()).unwrap_or(1);
        // skip SCODE(3), DCODE(4)
        let area: u32 = f1.get(5).and_then(|t| t.parse().ok()).unwrap_or(1);
        let zone: u32 = f1.get(6).and_then(|t| t.parse().ok()).unwrap_or(1);
        let owner: u32 = f1.get(7).and_then(|t| t.parse().ok()).unwrap_or(1);
        // skip TCODE(8), BCODE(9)
        let pf = |s: &str| s.trim().parse::<f64>().unwrap_or(0.0);
        let mbase: f64 = f1.get(10).map(|t| pf(t)).unwrap_or(0.0);
        let rate_kv: f64 = f1.get(11).map(|t| pf(t)).unwrap_or(0.0);
        // skip PCODE(12)
        let pset: f64 = f1.get(13).map(|t| pf(t)).unwrap_or(0.0);
        let h: f64 = f1.get(14).map(|t| pf(t)).unwrap_or(0.0);
        let a: f64 = f1.get(15).map(|t| pf(t)).unwrap_or(0.0);
        let b: f64 = f1.get(16).map(|t| pf(t)).unwrap_or(0.0);
        let d: f64 = f1.get(17).map(|t| pf(t)).unwrap_or(0.0);
        let e: f64 = f1.get(18).map(|t| pf(t)).unwrap_or(0.0);
        let f_coeff: f64 = f1.get(19).map(|t| pf(t)).unwrap_or(0.0);

        let ra: f64 = f2.first().map(|t| pf(t)).unwrap_or(0.0);
        let xa: f64 = f2.get(1).map(|t| pf(t)).unwrap_or(0.0);
        let xm: f64 = f2.get(2).map(|t| pf(t)).unwrap_or(0.0);
        let r1: f64 = f2.get(3).map(|t| pf(t)).unwrap_or(0.0);
        let x1: f64 = f2.get(4).map(|t| pf(t)).unwrap_or(0.0);
        let r2: f64 = f2.get(5).map(|t| pf(t)).unwrap_or(0.0);
        let x2: f64 = f2.get(6).map(|t| pf(t)).unwrap_or(0.0);
        let x3: f64 = f2.get(7).map(|t| pf(t)).unwrap_or(0.0);

        machines.push(InductionMachine {
            bus,
            id,
            in_service: stat != 0,
            mbase,
            rate_kv,
            pset,
            h,
            a,
            b,
            d,
            e,
            f_coeff,
            ra,
            xa,
            xm,
            r1,
            x1,
            r2,
            x2,
            x3,
            area,
            zone,
            owner,
            load_id: None,
        });
    }

    machines
}

/// ```text
/// ISUB, 'NAME', LATI, LONG, SGR
///   / BEGIN SUBSTATION NODE DATA
///     INODE, 'NAME', IBUS, STATUS, VM, VA
///   0 / END OF SUBSTATION NODE DATA, BEGIN SUBSTATION SWITCHING DEVICE DATA
///     NI, NJ, 'CKT', 'NAME', TYPE, STATUS, NSTAT, X, RATE1, RATE2, RATE3
///   0 / END OF SUBSTATION SWITCHING DEVICE DATA, BEGIN SUBSTATION TERMINAL DATA
///     ISUB, INODE, TYPE, ...
///   0 / END OF SUBSTATION TERMINAL DATA
/// ```
fn parse_substation_data_section(
    lines: &[&str],
    start: usize,
    bus_basekv: &HashMap<u32, f64>,
    sys_switch_devices: &[RawSystemSwitchDevice],
) -> NodeBreakerTopology {
    let mut substations = Vec::new();
    let mut voltage_levels = Vec::new();
    let mut connectivity_nodes = Vec::new();
    let mut busbar_sections = Vec::new();
    let mut switches = Vec::new();
    let mut terminal_connections = Vec::new();
    let bays = Vec::new();

    let mut pos = start;

    // Track unique (substation_id, base_kv) pairs for voltage levels.
    let mut vl_set: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();

    // Accumulate node→bus associations across all substations for topology reduction.
    let mut all_connectivity_node_to_bus: HashMap<String, u32> = HashMap::new();

    while pos < lines.len() {
        let line = lines[pos].trim();
        if line.is_empty() || line.starts_with("@!") {
            pos += 1;
            continue;
        }
        if is_section_end(line) {
            // End of entire SUBSTATION DATA section.
            pos += 1;
            break;
        }
        if line.starts_with('Q') || line.eq_ignore_ascii_case("q") {
            break;
        }

        // Parse substation header: ISUB, 'NAME', LATI, LONG, SGR
        let fields = tokenize_record(line);
        if fields.is_empty() {
            pos += 1;
            continue;
        }

        let isub: u32 = fields[0].parse().unwrap_or(0);
        let sub_name = fields.get(1).cloned().unwrap_or_default();
        let sub_id = format!("SUB_{isub}");

        substations.push(SubstationData {
            id: sub_id.clone(),
            name: sub_name,
            region: None,
        });

        pos += 1;

        // Skip "/ BEGIN SUBSTATION NODE DATA" marker line.
        while pos < lines.len() {
            let l = lines[pos].trim();
            if l.is_empty() || l.starts_with("@!") || l.starts_with('/') {
                pos += 1;
                continue;
            }
            break;
        }

        // Parse node records until section end.
        // Node fields: INODE, 'NAME', IBUS, STATUS, VM, VA
        let mut sub_nodes: Vec<(u32, String, u32)> = Vec::new(); // (inode, name, ibus)

        while pos < lines.len() {
            let l = lines[pos].trim();
            if l.is_empty() || l.starts_with("@!") {
                pos += 1;
                continue;
            }
            if is_section_end(l) {
                pos += 1;
                break;
            }
            let nf = tokenize_record(l);
            if nf.len() >= 3 {
                let inode: u32 = nf[0].parse().unwrap_or(0);
                let node_name = nf.get(1).cloned().unwrap_or_default();
                let ibus: u32 = nf[2].parse().unwrap_or(0);

                let cn_id = format!("SUB_{isub}_N{inode}");
                let base_kv = bus_basekv.get(&ibus).copied().unwrap_or(1.0);
                let vl_id = format!("VL_{sub_id}_{}", (base_kv * 10.0) as u64);

                // Create VoltageLevel if not yet seen.
                let vl_key = (sub_id.clone(), (base_kv * 10.0) as u64);
                if vl_set.insert(vl_key) {
                    voltage_levels.push(VoltageLevel {
                        id: vl_id.clone(),
                        name: format!("{base_kv} kV"),
                        substation_id: sub_id.clone(),
                        base_kv,
                    });
                }

                connectivity_nodes.push(ConnectivityNode {
                    id: cn_id.clone(),
                    name: node_name.clone(),
                    voltage_level_id: vl_id,
                });

                // Record CN→bus mapping for topology.
                all_connectivity_node_to_bus.insert(cn_id.clone(), ibus);

                // If the node name contains "NB" (busbar), create a BusbarSection.
                if node_name.contains("NB") || node_name.contains("BB") {
                    busbar_sections.push(BusbarSection {
                        id: format!("BB_{cn_id}"),
                        name: node_name.clone(),
                        connectivity_node_id: cn_id.clone(),
                        ip_max: None,
                    });
                }

                sub_nodes.push((inode, cn_id, ibus));
            }
            pos += 1;
        }

        // Build node-id lookup for this substation.
        let node_cn: HashMap<u32, &str> = sub_nodes
            .iter()
            .map(|(inode, cn_id, _)| (*inode, cn_id.as_str()))
            .collect();

        // Parse switching device records until section end.
        // Fields: NI, NJ, 'CKT', 'NAME', TYPE, STATUS, NSTAT, X, RATE1, RATE2, RATE3
        while pos < lines.len() {
            let l = lines[pos].trim();
            if l.is_empty() || l.starts_with("@!") {
                pos += 1;
                continue;
            }
            if is_section_end(l) {
                pos += 1;
                break;
            }
            let sf = tokenize_record(l);
            if sf.len() >= 6 {
                let ni: u32 = sf[0].parse().unwrap_or(0);
                let nj: u32 = sf[1].parse().unwrap_or(0);
                let ckt = sf.get(2).cloned().unwrap_or_else(|| "1".into());
                let sw_name = sf.get(3).cloned().unwrap_or_default();
                let device_type: u32 = sf.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);
                let status: u32 = sf.get(5).and_then(|s| s.parse().ok()).unwrap_or(1);
                let normal_status: u32 = sf.get(6).and_then(|s| s.parse().ok()).unwrap_or(1);
                let rate1: f64 = sf.get(8).and_then(|s| s.parse().ok()).unwrap_or(0.0);

                let cn1 = node_cn.get(&ni).unwrap_or(&"").to_string();
                let cn2 = node_cn.get(&nj).unwrap_or(&"").to_string();
                if !cn1.is_empty() && !cn2.is_empty() {
                    let sw_id = format!("SW_{sub_id}_N{ni}_N{nj}_{ckt}");
                    let rated_current = if rate1 > 0.0 { Some(rate1) } else { None };

                    switches.push(SwitchDevice {
                        id: sw_id,
                        name: sw_name,
                        switch_type: psse_switch_type(device_type),
                        cn1_id: cn1,
                        cn2_id: cn2,
                        open: status == 0,
                        normal_open: normal_status == 0,
                        retained: false,
                        rated_current,
                    });
                }
            }
            pos += 1;
        }

        // Parse terminal records until section end.
        // Terminal formats vary by type:
        //   ISUB, INODE, 'M', 'CKT'              — machine (generator)
        //   ISUB, INODE, 'L', 'CKT'              — load
        //   ISUB, INODE, 'B', JBUS, 'CKT'        — branch/transformer to bus JBUS
        //   ISUB, INODE, 'X', JBUS, 'CKT'        — (same as B for 2W xfmr)
        //   ISUB, INODE, 'S', 'CKT'              — switched shunt
        //   ISUB, INODE, 'F', 'CKT'              — fixed shunt
        while pos < lines.len() {
            let l = lines[pos].trim();
            if l.is_empty() || l.starts_with("@!") {
                pos += 1;
                continue;
            }
            if is_section_end(l) {
                pos += 1;
                break;
            }
            let tf = tokenize_record(l);
            if tf.len() >= 3 {
                let _term_sub: u32 = tf[0].parse().unwrap_or(0);
                let term_node: u32 = tf[1].parse().unwrap_or(0);
                let term_type = tf.get(2).cloned().unwrap_or_default();

                let cn_id = node_cn.get(&term_node).unwrap_or(&"").to_string();
                if cn_id.is_empty() {
                    pos += 1;
                    continue;
                }

                let (equip_id, equip_class, seq) = match term_type.as_str() {
                    "M" => {
                        let ckt = tf.get(3).cloned().unwrap_or_else(|| "1".into());
                        // Find the bus for this node.
                        let ibus = sub_nodes
                            .iter()
                            .find(|(n, _, _)| *n == term_node)
                            .map(|(_, _, b)| *b)
                            .unwrap_or(0);
                        (
                            format!("GEN_{ibus}_{ckt}"),
                            "SynchronousMachine".into(),
                            1u32,
                        )
                    }
                    "L" => {
                        let ckt = tf.get(3).cloned().unwrap_or_else(|| "1".into());
                        let ibus = sub_nodes
                            .iter()
                            .find(|(n, _, _)| *n == term_node)
                            .map(|(_, _, b)| *b)
                            .unwrap_or(0);
                        (format!("LOAD_{ibus}_{ckt}"), "EnergyConsumer".into(), 1)
                    }
                    "B" | "X" => {
                        let jbus: u32 = tf.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
                        let ckt = tf.get(4).cloned().unwrap_or_else(|| "1".into());
                        let ibus = sub_nodes
                            .iter()
                            .find(|(n, _, _)| *n == term_node)
                            .map(|(_, _, b)| *b)
                            .unwrap_or(0);
                        let class = if term_type == "X" {
                            "PowerTransformer"
                        } else {
                            "ACLineSegment"
                        };
                        (format!("BR_{ibus}_{jbus}_{ckt}"), class.into(), 1)
                    }
                    _ => {
                        // Fixed shunt, switched shunt, etc. — still record the terminal.
                        let ckt = tf.get(3).cloned().unwrap_or_else(|| "1".into());
                        let ibus = sub_nodes
                            .iter()
                            .find(|(n, _, _)| *n == term_node)
                            .map(|(_, _, b)| *b)
                            .unwrap_or(0);
                        (
                            format!("EQUIP_{ibus}_{term_type}_{ckt}"),
                            term_type.clone(),
                            1,
                        )
                    }
                };

                let term_id = format!("T_{cn_id}_{equip_id}");
                terminal_connections.push(TerminalConnection {
                    terminal_id: term_id,
                    equipment_id: equip_id,
                    equipment_class: equip_class,
                    sequence_number: seq,
                    connectivity_node_id: cn_id,
                });
            }
            pos += 1;
        }
    }
    let _ = pos; // suppress unused-assignment warning

    // Incorporate system-level switching devices (inter-bus).
    // These connect buses rather than substation-local nodes, so we create
    // synthetic CNs for each bus endpoint.
    for ssd in sys_switch_devices {
        let cn1_id = format!("SYSBUS_{}", ssd.bus_i);
        let cn2_id = format!("SYSBUS_{}", ssd.bus_j);
        let sw_id = format!("SYS_SW_{}_{}_{}", ssd.bus_i, ssd.bus_j, ssd.ckt);

        // Ensure CNs exist (may have been created by a prior sys device).
        if !connectivity_nodes.iter().any(|cn| cn.id == cn1_id) {
            let base_kv = bus_basekv.get(&ssd.bus_i).copied().unwrap_or(1.0);
            connectivity_nodes.push(ConnectivityNode {
                id: cn1_id.clone(),
                name: format!("Bus_{}", ssd.bus_i),
                voltage_level_id: format!("VL_SYS_{}", (base_kv * 10.0) as u64),
            });
        }
        if !connectivity_nodes.iter().any(|cn| cn.id == cn2_id) {
            let base_kv = bus_basekv.get(&ssd.bus_j).copied().unwrap_or(1.0);
            connectivity_nodes.push(ConnectivityNode {
                id: cn2_id.clone(),
                name: format!("Bus_{}", ssd.bus_j),
                voltage_level_id: format!("VL_SYS_{}", (base_kv * 10.0) as u64),
            });
        }

        let rated_current = if ssd.rate1 > 0.0 {
            Some(ssd.rate1)
        } else {
            None
        };
        switches.push(SwitchDevice {
            id: sw_id,
            name: ssd.name.clone(),
            switch_type: psse_switch_type(ssd.device_type),
            cn1_id,
            cn2_id,
            open: ssd.status == 0,
            normal_open: ssd.normal_status == 0,
            retained: false,
            rated_current,
        });
    }

    // Build topology reduction from the accumulated node→bus associations.
    let mut bus_to_connectivity_nodes: HashMap<u32, Vec<String>> = HashMap::new();
    for (cn_id, &bus) in &all_connectivity_node_to_bus {
        bus_to_connectivity_nodes
            .entry(bus)
            .or_default()
            .push(cn_id.clone());
    }

    // Consumed switches: closed switches whose two CNs map to the same bus.
    let consumed_switch_ids: Vec<String> = switches
        .iter()
        .filter(|sw| !sw.open && !sw.retained)
        .filter(|sw| {
            all_connectivity_node_to_bus.get(&sw.cn1_id)
                == all_connectivity_node_to_bus.get(&sw.cn2_id)
                && all_connectivity_node_to_bus.contains_key(&sw.cn1_id)
        })
        .map(|sw| sw.id.clone())
        .collect();

    // Isolated CNs: no terminal connections.
    let connected_cns: std::collections::HashSet<&str> = terminal_connections
        .iter()
        .map(|tc| tc.connectivity_node_id.as_str())
        .collect();
    let isolated_connectivity_node_ids: Vec<String> = connectivity_nodes
        .iter()
        .filter(|cn| !connected_cns.contains(cn.id.as_str()))
        .map(|cn| cn.id.clone())
        .collect();

    let reduction = if all_connectivity_node_to_bus.is_empty() {
        None
    } else {
        Some(TopologyMapping {
            connectivity_node_to_bus: all_connectivity_node_to_bus,
            bus_to_connectivity_nodes,
            consumed_switch_ids,
            isolated_connectivity_node_ids,
        })
    };

    tracing::debug!(
        substations = substations.len(),
        nodes = connectivity_nodes.len(),
        switches = switches.len(),
        terminals = terminal_connections.len(),
        "parsed PSS/E substation data"
    );

    match reduction {
        Some(reduction) => NodeBreakerTopology::new(
            substations,
            voltage_levels,
            bays,
            connectivity_nodes,
            busbar_sections,
            switches,
            terminal_connections,
        )
        .with_mapping(reduction),
        None => NodeBreakerTopology::new(
            substations,
            voltage_levels,
            bays,
            connectivity_nodes,
            busbar_sections,
            switches,
            terminal_connections,
        ),
    }
}

/// Build a minimal NodeBreakerTopology from system-level switching devices only
/// (when no SUBSTATION DATA section is present).
fn build_sys_switch_model(
    sys_devices: &[RawSystemSwitchDevice],
    _network: &Network,
    bus_basekv: &HashMap<u32, f64>,
) -> NodeBreakerTopology {
    let mut connectivity_nodes = Vec::new();
    let mut switches = Vec::new();
    let mut voltage_levels = Vec::new();
    let mut vl_set: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut cn_set: std::collections::HashSet<u32> = std::collections::HashSet::new();

    for ssd in sys_devices {
        for &bus in &[ssd.bus_i, ssd.bus_j] {
            if cn_set.insert(bus) {
                let base_kv = bus_basekv.get(&bus).copied().unwrap_or(1.0);
                let vl_key = (base_kv * 10.0) as u64;
                if vl_set.insert(vl_key) {
                    voltage_levels.push(VoltageLevel {
                        id: format!("VL_SYS_{vl_key}"),
                        name: format!("{base_kv} kV"),
                        substation_id: "SYS".into(),
                        base_kv,
                    });
                }
                connectivity_nodes.push(ConnectivityNode {
                    id: format!("SYSBUS_{bus}"),
                    name: format!("Bus_{bus}"),
                    voltage_level_id: format!("VL_SYS_{vl_key}"),
                });
            }
        }

        let sw_id = format!("SYS_SW_{}_{}_{}", ssd.bus_i, ssd.bus_j, ssd.ckt);
        let rated_current = if ssd.rate1 > 0.0 {
            Some(ssd.rate1)
        } else {
            None
        };
        switches.push(SwitchDevice {
            id: sw_id,
            name: ssd.name.clone(),
            switch_type: psse_switch_type(ssd.device_type),
            cn1_id: format!("SYSBUS_{}", ssd.bus_i),
            cn2_id: format!("SYSBUS_{}", ssd.bus_j),
            open: ssd.status == 0,
            normal_open: ssd.normal_status == 0,
            retained: false,
            rated_current,
        });
    }

    NodeBreakerTopology::new(
        vec![SubstationData {
            id: "SYS".into(),
            name: "System".into(),
            region: None,
        }],
        voltage_levels,
        Vec::new(),
        connectivity_nodes,
        Vec::new(),
        switches,
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(dead_code)]
    fn data_available() -> bool {
        if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
            return std::path::Path::new(&p).exists();
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/data")
            .exists()
    }
    #[allow(dead_code)]
    fn test_data_dir() -> std::path::PathBuf {
        if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
            return std::path::PathBuf::from(p);
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/data")
    }

    #[test]
    fn test_tokenize_comma_delimited() {
        let tokens = tokenize_record("1, 'BUS 1   ', 138.0, 1, 1, 1, 1, 1.050, -10.5");
        assert_eq!(tokens[0], "1");
        assert_eq!(tokens[1], "'BUS 1   '");
        assert_eq!(tokens[2], "138.0");
        assert_eq!(tokens[3], "1");
        assert_eq!(tokens.len(), 9);
    }

    #[test]
    fn test_tokenize_with_comment() {
        let tokens = tokenize_record("1, 2, 3 / this is a comment");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[2], "3");
    }

    #[test]
    fn test_section_end() {
        assert!(is_section_end("0 / END OF BUS DATA"));
        assert!(is_section_end("0"));
        assert!(is_section_end(" 0 / END OF DATA"));
        assert!(!is_section_end("10, 'BUS', 138.0"));
        // PE-01 regression: "0," must NOT be treated as a section terminator
        assert!(!is_section_end("0,5,'1 ',0.01,0.1,0.0,100.0"));
        assert!(!is_section_end("0, 5, '1 '"));
        // Other patterns that must not match
        assert!(!is_section_end("01"));
        assert!(!is_section_end("0.0"));
        assert!(!is_section_end("0.01, 0.1"));
    }

    #[test]
    fn test_parse_f64_rejects_nan_inf() {
        // PE-04: parse_f64 must reject NaN and Inf strings
        assert!(parse_f64("NaN", 1, "test").is_err());
        assert!(parse_f64("nan", 1, "test").is_err());
        assert!(parse_f64("inf", 1, "test").is_err());
        assert!(parse_f64("Inf", 1, "test").is_err());
        assert!(parse_f64("-inf", 1, "test").is_err());
        assert!(parse_f64("-Inf", 1, "test").is_err());
        // Zero and normal values must still be accepted (0.0 = unconstrained rating)
        assert_eq!(parse_f64("0.0", 1, "rate").unwrap(), 0.0);
        assert_eq!(parse_f64("0", 1, "rate").unwrap(), 0.0);
        assert!((parse_f64("1.5", 1, "r").unwrap() - 1.5).abs() < 1e-12);
        assert!((parse_f64("-3.15", 1, "x").unwrap() + 3.15).abs() < 1e-12);
    }

    #[test]
    fn test_parse_switched_shunt_rejects_truncated_record() {
        let lines = vec!["1, 1", "Q"];
        let err = parse_switched_shunt_section(&lines, 0).unwrap_err();
        match err {
            PsseError::Parse { line, message } => {
                assert_eq!(line, 1);
                assert!(message.contains("switched shunt record is truncated"));
            }
            other => panic!("expected parse error for truncated switched shunt, got {other:?}"),
        }
    }

    #[test]
    fn test_section_end_bus0_branch_not_truncated() {
        // PE-01 regression: a branch from bus 0 should not trigger early section
        // termination because "0," looks like a section end to the old code.
        let raw = r#"0,   100.00, 33, 0, 0, 60.00
Test Case
Test Case
    0,'BUS0    ',  138.0000,3,   1,   1,   1, 1.06000,   0.0000
    5,'BUS5    ',  138.0000,1,   1,   1,   1, 1.00000,   0.0000
0 / END OF BUS DATA
0 / END OF LOAD DATA
0 / END OF FIXED SHUNT DATA
    5,'1 ',  100.000,    0.000,  300.000, -300.000, 1.06000,    0,  100.000,  0.0,  1.0,  0.0,  0.0,  1.0,1, 100.0,  250.000,    10.000
0 / END OF GENERATOR DATA
    0,     5,'1 ', 0.01000, 0.10000, 0.02000,  100.00,  100.00,  100.00,  0.00000,  0.00000,  0.00000,  0.00000,1,1,   0.000,0,0,0,0,0
0 / END OF NON-TRANSFORMER BRANCH DATA
0 / END OF TRANSFORMER DATA
Q
"#;
        let net =
            parse_str(raw).expect("bus-0 branch should parse without early section termination");
        assert_eq!(net.n_branches(), 1, "branch from bus-0 must not be dropped");
        assert_eq!(net.branches[0].from_bus, 0);
        assert_eq!(net.branches[0].to_bus, 5);
    }

    #[test]
    fn test_parse_truncated_transformer_errors() {
        let raw = r#"0,   100.00, 33, 0, 0, 60.00
Test Case
Test Case Heading 2
    1,'BUS1    ',  138.0000,3,   1,   1,   1, 1.06000,   0.0000
    2,'BUS2    ',  138.0000,1,   1,   1,   1, 1.00000,   0.0000
0 / END OF BUS DATA
0 / END OF LOAD DATA
0 / END OF FIXED SHUNT DATA
    1,'1 ',  100.000,    0.000,  300.000, -300.000, 1.06000,    0,  100.000,  0.0,  1.0,  0.0,  0.0,  1.0,1, 100.0,  250.000,    10.000
0 / END OF GENERATOR DATA
    1,     2,'1 ', 0.01000, 0.10000, 0.02000,  100.00,  100.00,  100.00,  0.00000,  0.00000,  0.00000,  0.00000,1,1,   0.000,0,0,0,0,0
0 / END OF NON-TRANSFORMER BRANCH DATA
    1,     2,'1 '
0 / END OF TRANSFORMER DATA
Q
"#;
        let err = parse_str(raw).unwrap_err();
        match err {
            PsseError::Parse { message, .. } => {
                assert!(message.contains("truncated transformer record 1"));
            }
            other => panic!("expected transformer truncation error, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_area_schedule_section_rejects_invalid_numeric_fields() {
        let lines = vec!["1, BAD, 10.0, 5.0, 'AREA 1'", "Q"];
        let err = parse_area_schedule_section(&lines, 0).unwrap_err();
        match err {
            PsseError::Parse { line, message } => {
                assert_eq!(line, 1);
                assert!(message.contains("ISW"));
            }
            other => panic!("expected parse error for invalid area schedule row, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_area_schedule_section_rejects_truncated_rows() {
        let lines = vec!["1, 2, 10.0, 5.0", "Q"];
        let err = parse_area_schedule_section(&lines, 0).unwrap_err();
        match err {
            PsseError::Parse { line, message } => {
                assert_eq!(line, 1);
                assert!(message.contains("truncated"));
            }
            other => panic!("expected parse error for truncated area schedule row, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_minimal_raw() {
        // Minimal PSS/E RAW v33 file with 2 buses, 1 generator, 1 branch
        let raw = r#"0,   100.00, 33, 0, 0, 60.00 / PSS/E 33 case
Test Case
Test Case Heading 2
    1,'BUS1    ',  138.0000,3,   1,   1,   1, 1.06000,   0.0000
    2,'BUS2    ',  138.0000,1,   1,   1,   1, 1.00000,   0.0000
0 / END OF BUS DATA
    2,'1 ',1,   1,   1,   50.000,    20.000,    0.000,    0.000,    0.000,    0.000,   1,1,0,1,0,0
0 / END OF LOAD DATA
0 / END OF FIXED SHUNT DATA
    1,'1 ',  100.000,    0.000,  300.000, -300.000, 1.06000,    0,  100.000,  0.0,  1.0,  0.0,  0.0,  1.0,1, 100.0,  250.000,    10.000
0 / END OF GENERATOR DATA
    1,     2,'1 ', 0.01000, 0.10000, 0.02000,  100.00,  100.00,  100.00,  0.00000,  0.00000,  0.00000,  0.00000,1,1,   0.000,0,0,0,0,0
0 / END OF NON-TRANSFORMER BRANCH DATA
0 / END OF TRANSFORMER DATA
Q
"#;
        let net = parse_str(raw).expect("failed to parse minimal PSS/E RAW");

        assert_eq!(net.base_mva, 100.0);
        assert_eq!(net.n_buses(), 2);
        assert_eq!(net.generators.len(), 1);
        assert_eq!(net.n_branches(), 1);

        // Check bus data
        assert_eq!(net.buses[0].number, 1);
        assert_eq!(net.buses[0].bus_type, BusType::Slack);
        assert!((net.buses[0].voltage_magnitude_pu - 1.06).abs() < 1e-10);
        assert!((net.buses[0].base_kv - 138.0).abs() < 1e-10);

        assert_eq!(net.buses[1].number, 2);
        assert_eq!(net.buses[1].bus_type, BusType::PQ);

        // Check load was applied to bus 2 (via Load objects)
        let bus_pd = net.bus_load_p_mw();
        let bus_qd = net.bus_load_q_mvar();
        assert!((bus_pd[1] - 50.0).abs() < 1e-10);
        assert!((bus_qd[1] - 20.0).abs() < 1e-10);

        // Check generator
        assert_eq!(net.generators[0].bus, 1);
        assert!((net.generators[0].p - 100.0).abs() < 1e-10);
        assert!((net.generators[0].voltage_setpoint_pu - 1.06).abs() < 1e-10);
        assert!(net.generators[0].in_service);

        // Check branch
        assert_eq!(net.branches[0].from_bus, 1);
        assert_eq!(net.branches[0].to_bus, 2);
        assert!((net.branches[0].r - 0.01).abs() < 1e-10);
        assert!((net.branches[0].x - 0.10).abs() < 1e-10);
        assert!((net.branches[0].b - 0.02).abs() < 1e-10);
        assert!((net.branches[0].tap - 1.0).abs() < 1e-10);
        assert!(net.branches[0].in_service);
    }

    #[test]
    fn test_parse_with_transformer() {
        let raw = r#"0,   100.00, 33, 0, 0, 60.00
Test Case
Test Case
    1,'BUS1    ',  345.0000,3,   1,   1,   1, 1.06000,   0.0000
    2,'BUS2    ',  138.0000,1,   1,   1,   1, 1.00000,   0.0000
0 / END OF BUS DATA
0 / END OF LOAD DATA
0 / END OF FIXED SHUNT DATA
    1,'1 ',  100.000,    0.000,  300.000, -300.000, 1.06000,    0,  100.000,  0.0,  1.0,  0.0,  0.0,  1.0,1, 100.0,  250.000,    10.000
0 / END OF GENERATOR DATA
0 / END OF NON-TRANSFORMER BRANCH DATA
    1,     2,     0,'1 ',1,1,1,  0.00000,  0.00000,2,'            ',1,1,1.00000,0,1.00000,0,1.00000,0,0,  0.00000,'            '
   0.00000, 0.10000,  100.00
   1.05000,    0.000,   0.000,  100.00,    0.00,    0.00,   0,   0, 1.10000, 0.90000, 1.10000, 0.90000,  33,   0, 0.00000, 0.00000,  0.000
   1.00000,    0.000
0 / END OF TRANSFORMER DATA
Q
"#;
        let net = parse_str(raw).expect("failed to parse PSS/E with transformer");

        assert_eq!(net.n_buses(), 2);
        assert_eq!(net.n_branches(), 1); // just the transformer
        assert_eq!(net.generators.len(), 1);

        let xfmr = &net.branches[0];
        assert_eq!(xfmr.from_bus, 1);
        assert_eq!(xfmr.to_bus, 2);
        assert!((xfmr.x - 0.10).abs() < 1e-10);
        // CW=1: tap = windv1/windv2 = 1.05/1.0 = 1.05
        assert!((xfmr.tap - 1.05).abs() < 1e-10);
        assert!(xfmr.in_service);
    }

    #[test]
    fn test_parse_with_fixed_shunt() {
        let raw = r#"0,   100.00, 33, 0, 0, 60.00
Test
Test
    1,'BUS1    ',  138.0000,3,   1,   1,   1, 1.06000,   0.0000
    2,'BUS2    ',  138.0000,1,   1,   1,   1, 1.00000,   0.0000
0 / END OF BUS DATA
0 / END OF LOAD DATA
    2,'1 ',1,   0.000,  19.000
0 / END OF FIXED SHUNT DATA
    1,'1 ',  100.000,    0.000,  300.000, -300.000, 1.06000,    0,  100.000,  0.0,  1.0,  0.0,  0.0,  1.0,1, 100.0,  250.000,    10.000
0 / END OF GENERATOR DATA
    1,     2,'1 ', 0.01000, 0.10000, 0.02000,  100.00,  100.00,  100.00,  0.00000,  0.00000,  0.00000,  0.00000,1,1,   0.000,0,0,0,0,0
0 / END OF NON-TRANSFORMER BRANCH DATA
0 / END OF TRANSFORMER DATA
Q
"#;
        let net = parse_str(raw).expect("failed to parse PSS/E with shunt");

        // Fixed shunt should be applied to bus 2
        let bus2 = net.buses.iter().find(|b| b.number == 2).unwrap();
        assert!((bus2.shunt_susceptance_mvar - 19.0).abs() < 1e-10);
    }

    #[test]
    fn test_parse_status_rejects_empty_and_unknown() {
        assert!(parse_status("", 1, "STAT").is_err());
        assert!(parse_status("maybe", 1, "STAT").is_err());
        assert_eq!(parse_status("1", 1, "STAT").unwrap(), 1);
        assert_eq!(parse_status("0", 1, "STAT").unwrap(), 0);
    }

    #[test]
    fn test_dc_converter_record_requires_status_field() {
        let fields = vec![
            "1".to_string(),
            "1".to_string(),
            "30".to_string(),
            "5".to_string(),
            "0.1".to_string(),
            "0.1".to_string(),
            "230".to_string(),
            "1".to_string(),
            "1".to_string(),
            "1".to_string(),
            "1".to_string(),
            "1".to_string(),
            "".to_string(),
        ];
        assert!(
            parse_dc_converter_record(&fields, 1).is_err(),
            "blank IC must be rejected"
        );
    }

    #[test]
    fn test_vsc_converter_record_requires_state_field() {
        let fields = vec![
            "1".to_string(),
            "1".to_string(),
            "2".to_string(),
            "0.0".to_string(),
            "1.0".to_string(),
            "0.0".to_string(),
            "0.0".to_string(),
            "0.0".to_string(),
            "100.0".to_string(),
            "-100.0".to_string(),
            "50.0".to_string(),
            "-50.0".to_string(),
            "1.0".to_string(),
            "".to_string(),
        ];
        assert!(
            parse_vsc_converter_record(&fields, 1).is_err(),
            "blank STATE must be rejected"
        );
    }

    #[test]
    fn test_parse_ieee14_raw() {
        let path = test_data_dir().join("IEEE_14_bus.raw");
        if !path.exists() {
            return; // skip if test data not downloaded
        }
        let net = parse_file(&path).expect("failed to parse IEEE 14 bus RAW");
        assert_eq!(net.n_buses(), 14);
        assert_eq!(net.n_branches(), 20);
        assert_eq!(net.generators.len(), 5);
        assert!(net.total_load_mw() > 250.0);
    }

    #[test]
    fn test_parse_ieee30_raw() {
        let path = test_data_dir().join("IEEE_30_bus.raw");
        if !path.exists() {
            return;
        }
        let net = parse_file(&path).expect("failed to parse IEEE 30 bus RAW");
        assert_eq!(net.n_buses(), 30);
        assert_eq!(net.n_branches(), 41);
        assert_eq!(net.generators.len(), 6);
    }

    #[test]
    fn test_parse_ieee57_raw() {
        let path = test_data_dir().join("IEEE_57_bus.raw");
        if !path.exists() {
            return;
        }
        let net = parse_file(&path).expect("failed to parse IEEE 57 bus RAW");
        assert_eq!(net.n_buses(), 57);
        assert_eq!(net.n_branches(), 80);
        assert_eq!(net.generators.len(), 7);
    }

    #[test]
    fn test_parse_ieee118_raw() {
        let path = test_data_dir().join("IEEE_118_bus.raw");
        if !path.exists() {
            return;
        }
        let net = parse_file(&path).expect("failed to parse IEEE 118 bus RAW");
        assert_eq!(net.n_buses(), 118);
        assert_eq!(net.n_branches(), 186);
        assert_eq!(net.generators.len(), 54);
    }

    /// PSS/E v36 file with @!IC header line and @! section annotations (240-bus WECC).
    #[test]
    fn test_parse_wecc_240_v36_with_annotations() {
        let path = test_data_dir()
            .join("raw")
            .join("240busWECC_2018_PSS_PQLoad_fixedshunt_noremotebus_yuan_v36.raw");
        if !path.exists() {
            return;
        }
        let net = parse_file(&path).expect("failed to parse WECC 240-bus v36 RAW");
        // The "240-bus WECC" model has 243 buses in the RAW file (240 network
        // buses + 3 generator step-up transformer low-side buses).
        assert_eq!(net.n_buses(), 243, "bus count");
        assert!(net.n_branches() > 0, "must have branches");
        assert!(!net.generators.is_empty(), "must have generators");
        // This particular v36 file does not include LATITUDE/LONGITUDE fields
        // in the bus records (PSS/E v35+ supports them but they are optional).
    }

    /// PSS/E v34 file with @! section annotations (240-bus WECC, fixed-shunt variant).
    #[test]
    fn test_parse_wecc_240_v34_with_annotations() {
        let path = test_data_dir()
            .join("raw")
            .join("240busWECC_2018_PSS_fixedshunt.raw");
        if !path.exists() {
            return;
        }
        let net = parse_file(&path).expect("failed to parse WECC 240-bus v34 RAW");
        // Same model as v36; both RAW files contain 243 buses (240 network + 3 GSU).
        assert_eq!(net.n_buses(), 243, "bus count");
        assert!(net.n_branches() > 0, "must have branches");
    }

    /// Cross-format validation: PSS/E RAW vs MATPOWER for IEEE 14 bus
    #[test]
    fn test_cross_format_ieee14_raw_vs_matpower() {
        let raw_path = test_data_dir().join("IEEE_14_bus.raw");
        let m_path = test_data_dir().join("case14.m");
        if !raw_path.exists() {
            return;
        }

        let raw_net = parse_file(&raw_path).expect("failed to parse RAW");
        let m_net = crate::matpower::load(&m_path).expect("failed to parse MATPOWER");

        // Same topology
        assert_eq!(raw_net.n_buses(), m_net.n_buses());
        assert_eq!(raw_net.n_branches(), m_net.n_branches());
        assert_eq!(raw_net.generators.len(), m_net.generators.len());

        // Same total load (within tolerance)
        assert!(
            (raw_net.total_load_mw() - m_net.total_load_mw()).abs() < 1.0,
            "Load mismatch: RAW={:.1}, MATPOWER={:.1}",
            raw_net.total_load_mw(),
            m_net.total_load_mw()
        );
    }

    #[test]
    fn test_psse_v35_substation_data() {
        // Minimal v35 RAW file with substation data section.
        let raw = r#"@!IC,SBASE,REV,XFRRAT,NXFRAT,BASFRQ
 0, 100.00, 35, 0, 0, 60.00
CASE HEADING 1
CASE HEADING 2
    1, 'BUS 1', 138.0, 3, 1, 1, 1, 1.060, 0.0, 1.1, 0.9
    2, 'BUS 2', 138.0, 1, 1, 1, 1, 1.045, -5.0, 1.1, 0.9
0 / END OF BUS DATA, BEGIN LOAD DATA
    2, '1 ', 1, 1, 1, 21.7, 12.7, 0.0, 0.0, 0.0, 0.0, 1
0 / END OF LOAD DATA, BEGIN FIXED SHUNT DATA
0 / END OF FIXED SHUNT DATA, BEGIN GENERATOR DATA
    1, '1 ', 40.0, 0.0, 999.0, -999.0, 1.060, 0, 100.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1, 100.0, 999.0, 0.0, 1, 1.0, 0, 0, 1.0, 0.0, 0, 0.0
0 / END OF GENERATOR DATA, BEGIN BRANCH DATA
    1, 2, '1 ', 0.01938, 0.05917, 0.05280, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1, 1, 0.0, 1, 1.0, 0, 1.0, 0, 1.0, 0, 1.0
0 / END OF BRANCH DATA, BEGIN SYSTEM SWITCHING DEVICE DATA
0 / END OF SYSTEM SWITCHING DEVICE DATA, BEGIN TRANSFORMER DATA
0 / END OF TRANSFORMER DATA, BEGIN AREA DATA
0 / END OF AREA DATA, BEGIN TWO-TERMINAL DC DATA
0 / END OF TWO-TERMINAL DC DATA, BEGIN VOLTAGE SOURCE CONVERTER DATA
0 / END OF VOLTAGE SOURCE CONVERTER DATA, BEGIN IMPEDANCE CORRECTION DATA
0 / END OF IMPEDANCE CORRECTION DATA, BEGIN MULTI-TERMINAL DC DATA
0 / END OF MULTI-TERMINAL DC DATA, BEGIN MULTI-SECTION LINE DATA
0 / END OF MULTI-SECTION LINE DATA, BEGIN ZONE DATA
0 / END OF ZONE DATA, BEGIN INTER-AREA TRANSFER DATA
0 / END OF INTER-AREA TRANSFER DATA, BEGIN OWNER DATA
0 / END OF OWNER DATA, BEGIN FACTS CONTROL DEVICE DATA
0 / END OF FACTS CONTROL DEVICE DATA, BEGIN SWITCHED SHUNT DATA
0 /END OF SWITCHED SHUNT DATA, BEGIN GNE DEVICE DATA
0 /END OF GNE DEVICE DATA, BEGIN INDUCTION MACHINE DATA
0 /END OF INDUCTION MACHINE DATA, BEGIN SUBSTATION DATA
	1,	'STATION 1',	0.0,	0.0,	0.1
	  / BEGIN SUBSTATION NODE DATA
		1,	'NB1',	1,	1,	1.0,	0.0
		2,	'NB2',	1,	1,	1.0,	0.0
		3,	'NL2',	1,	1,	1.0,	0.0
		4,	'NG1',	1,	1,	1.0,	0.0
	0 / END OF SUBSTATION NODE DATA, BEGIN SUBSTATION SWITCHING DEVICE DATA
		1,	2,	'1 ',	'Sw-BusBars',		2,	1,	1,	0,	0,	0,	0
		1,	3,	'1 ',	'Sw-Branch2',	2,	1,	1,	0,	0,	0,	0
		2,	4,	'1 ',	'Sw-Gen1',		2,	1,	1,	0,	0,	0,	0
	0 / END OF SUBSTATION SWITCHING DEVICE DATA, BEGIN SUBSTATION TERMINAL DATA
		1,	4,	'M', '1 '
		1,	3,	'B',  2,	'1 '
	0 / END OF SUBSTATION TERMINAL DATA
0 /END OF SUBSTATION DATA
Q
"#;

        let net = parse_str(raw).expect("failed to parse v35 RAW with substation data");
        assert_eq!(net.n_buses(), 2);
        assert_eq!(net.n_branches(), 1);
        assert_eq!(net.generators.len(), 1);

        // Verify NodeBreakerTopology is present and populated.
        let sm = net.topology.as_ref().expect("topology should be Some");
        assert_eq!(sm.substations.len(), 1, "should have 1 substation");
        assert_eq!(
            sm.connectivity_nodes.len(),
            4,
            "should have 4 connectivity nodes"
        );
        assert_eq!(sm.switches.len(), 3, "should have 3 switches");
        assert_eq!(
            sm.terminal_connections.len(),
            2,
            "should have 2 terminal connections"
        );

        // All switches should be closed (status=1 → open=false).
        for sw in &sm.switches {
            assert!(!sw.open, "switch {} should be closed", sw.id);
            assert_eq!(sw.switch_type, SwitchType::Breaker);
        }

        // Topology reduction: all 4 nodes map to bus 1 (all connected by closed
        // switches in the same substation, all mapped to IBUS=1).
        let reduction = sm
            .current_mapping()
            .expect("topology reduction should exist");
        assert_eq!(reduction.connectivity_node_to_bus.len(), 4);
        for cn_id in reduction.connectivity_node_to_bus.keys() {
            assert_eq!(
                reduction.connectivity_node_to_bus[cn_id], 1,
                "all nodes in station 1 should map to bus 1"
            );
        }

        // Busbar sections: NB1 and NB2 should be detected.
        assert_eq!(sm.busbar_sections.len(), 2, "should detect NB1 and NB2");
    }

    #[test]
    fn test_psse_v35_sys_switching_device() {
        // v35 file with a system switching device (inter-bus, between branches and transformers).
        let raw = r#"@!IC,SBASE,REV,XFRRAT,NXFRAT,BASFRQ
 0, 100.00, 35, 0, 0, 60.00
CASE HEADING 1
CASE HEADING 2
    1, 'BUS 1', 138.0, 3, 1, 1, 1, 1.060, 0.0, 1.1, 0.9
    2, 'BUS 2', 138.0, 1, 1, 1, 1, 1.045, -5.0, 1.1, 0.9
0 / END OF BUS DATA, BEGIN LOAD DATA
0 / END OF LOAD DATA, BEGIN FIXED SHUNT DATA
0 / END OF FIXED SHUNT DATA, BEGIN GENERATOR DATA
0 / END OF GENERATOR DATA, BEGIN BRANCH DATA
0 / END OF BRANCH DATA, BEGIN SYSTEM SWITCHING DEVICE DATA
    1, 2, '1 ', 'BRK_1_2', 2, 1, 1, 0.0001, 0.0, 0.0, 0.0
0 / END OF SYSTEM SWITCHING DEVICE DATA, BEGIN TRANSFORMER DATA
0 / END OF TRANSFORMER DATA, BEGIN AREA DATA
0 / END OF AREA DATA, BEGIN TWO-TERMINAL DC DATA
0 / END OF TWO-TERMINAL DC DATA, BEGIN VOLTAGE SOURCE CONVERTER DATA
0 / END OF VOLTAGE SOURCE CONVERTER DATA, BEGIN IMPEDANCE CORRECTION DATA
0 / END OF IMPEDANCE CORRECTION DATA, BEGIN MULTI-TERMINAL DC DATA
0 / END OF MULTI-TERMINAL DC DATA, BEGIN MULTI-SECTION LINE DATA
0 / END OF MULTI-SECTION LINE DATA, BEGIN ZONE DATA
0 / END OF ZONE DATA, BEGIN INTER-AREA TRANSFER DATA
0 / END OF INTER-AREA TRANSFER DATA, BEGIN OWNER DATA
0 / END OF OWNER DATA, BEGIN FACTS CONTROL DEVICE DATA
0 / END OF FACTS CONTROL DEVICE DATA, BEGIN SWITCHED SHUNT DATA
0 /END OF SWITCHED SHUNT DATA
Q
"#;

        let net = parse_str(raw).expect("failed to parse v35 with sys switching device");
        assert_eq!(net.n_buses(), 2);

        // Should have NodeBreakerTopology from system switching device.
        let sm = net
            .topology
            .as_ref()
            .expect("topology should be Some for sys switch devices");
        assert_eq!(sm.switches.len(), 1, "should have 1 sys switch device");
        assert_eq!(sm.switches[0].switch_type, SwitchType::Breaker);
        assert!(!sm.switches[0].open, "switch should be closed (status=1)");
        assert_eq!(
            sm.connectivity_nodes.len(),
            2,
            "should have 2 CNs for the two bus endpoints"
        );
    }

    /// Issue #28: INDUCTION MACHINE DATA section parsed from v35 RAW.
    #[test]
    fn test_parse_induction_machine_data() {
        let raw = r#" 0, 100.0, 35, 0, 0, 60.0 /  PSS/E 35 Raw Data
 Test Case
 Exported by Surge
     1,'BUS1',138.0,3,1,1,1,1.0,0.0,1
0 / END OF BUS DATA, BEGIN LOAD DATA
0 / END OF LOAD DATA, BEGIN FIXED SHUNT DATA
0 / END OF FIXED SHUNT DATA, BEGIN GENERATOR DATA
0 / END OF GENERATOR DATA, BEGIN BRANCH DATA
0 / END OF BRANCH DATA, BEGIN SYSTEM SWITCHING DEVICE DATA
0 / END OF SYSTEM SWITCHING DEVICE DATA, BEGIN TRANSFORMER DATA
0 / END OF TRANSFORMER DATA, BEGIN AREA DATA
0 / END OF AREA DATA, BEGIN TWO-TERMINAL DC DATA
0 / END OF TWO-TERMINAL DC DATA, BEGIN VOLTAGE SOURCE CONVERTER DATA
0 / END OF VOLTAGE SOURCE CONVERTER DATA, BEGIN IMPEDANCE CORRECTION DATA
0 / END OF IMPEDANCE CORRECTION DATA, BEGIN MULTI-TERMINAL DC DATA
0 / END OF MULTI-TERMINAL DC DATA, BEGIN MULTI-SECTION LINE DATA
0 / END OF MULTI-SECTION LINE DATA, BEGIN ZONE DATA
0 / END OF ZONE DATA, BEGIN INTER-AREA TRANSFER DATA
0 / END OF INTER-AREA TRANSFER DATA, BEGIN OWNER DATA
0 / END OF OWNER DATA, BEGIN FACTS CONTROL DEVICE DATA
0 / END OF FACTS CONTROL DEVICE DATA, BEGIN SWITCHED SHUNT DATA
0 /END OF SWITCHED SHUNT DATA, BEGIN GNE DEVICE DATA
0 /END OF GNE DEVICE DATA, BEGIN INDUCTION MACHINE DATA
    1,'M1',1,1,3,1,1,1,1,1,5.0,4.16,1,3.0,1.5,0.0,1.0,0.0,0.0,0.0
    0.01,0.05,2.5,0.03,0.08,0.0,0.0,0.0,1.0,0.0,1.2,0.0,0.0,0.0,1.0
0 /END OF INDUCTION MACHINE DATA, BEGIN SUBSTATION DATA
0 /END OF SUBSTATION DATA
Q
"#;
        let net = parse_str(raw).expect("failed to parse v35 RAW with induction machine");
        assert_eq!(
            net.induction_machines.len(),
            1,
            "expected 1 induction machine"
        );
        let m = &net.induction_machines[0];
        assert_eq!(m.bus, 1);
        assert_eq!(m.id.trim_matches('\'').trim(), "M1");
        assert!(m.in_service);
        assert!((m.mbase - 5.0).abs() < 1e-9, "mbase={}", m.mbase);
        assert!((m.h - 1.5).abs() < 1e-9, "H={}", m.h);
        assert!((m.ra - 0.01).abs() < 1e-9, "ra={}", m.ra);
        assert!((m.xm - 2.5).abs() < 1e-9, "xm={}", m.xm);
        assert!((m.r1 - 0.03).abs() < 1e-9, "r1={}", m.r1);
    }
}
