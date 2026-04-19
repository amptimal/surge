// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Refresh a Surge bundle from a PSS/E RAW source file.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use serde::Serialize;
use surge_network::Network;
use surge_network::network::{BusType, FuelParams, GenType, Generator};

#[derive(Parser, Debug)]
#[command(name = "refresh_activsg_psse")]
#[command(
    about = "Import a PSS/E RAW case, repair solve-time slack placement, and save Surge JSON"
)]
struct Args {
    /// Path to the PSS/E RAW source file.
    raw_path: PathBuf,
    /// Output path for the converted Surge artifact, e.g. .surge.json.zst.
    output_path: PathBuf,
    /// Optional PowerWorld AUX case used to backfill coordinates.
    #[arg(long)]
    aux_path: Option<PathBuf>,
    /// Optional MATPOWER case used to backfill economics and generator metadata.
    #[arg(long)]
    matpower_path: Option<PathBuf>,
    /// Optional JSON summary output path.
    #[arg(long)]
    summary_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct NetworkCounts {
    buses: usize,
    branches: usize,
    generators: usize,
    loads: usize,
    fixed_shunts: usize,
    facts_devices: usize,
    hvdc_links: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ComponentSlackRepair {
    component_size: usize,
    previous_slack_buses: Vec<u32>,
    chosen_slack_bus: u32,
    promoted_to_slack: bool,
    demoted_stale_slacks: Vec<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct RefreshSummary {
    raw_path: String,
    output_path: String,
    counts: NetworkCounts,
    slack_repairs: Vec<ComponentSlackRepair>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aux_backfill: Option<AuxBackfillSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    matpower_backfill: Option<MatpowerBackfillSummary>,
}

#[derive(Debug, Clone, Serialize)]
struct AuxBackfillSummary {
    aux_path: String,
    matched_buses: usize,
    buses_with_direct_aux_coordinates: usize,
    buses_with_substation_fallback_coordinates: usize,
    buses_with_coordinates_after_merge: usize,
    substations_with_coordinates: usize,
}

#[derive(Debug, Clone, Serialize)]
struct MatpowerBackfillSummary {
    matpower_path: String,
    matched_generators: usize,
    generators_with_cost: usize,
    generators_with_agc: usize,
    generators_with_ramping: usize,
    generators_with_reactive_capability: usize,
    generators_with_fuel_type: usize,
    generators_with_electrical_class: usize,
    generators_with_technology: usize,
    generators_with_source_technology_code: usize,
}

fn network_counts(network: &Network) -> NetworkCounts {
    NetworkCounts {
        buses: network.buses.len(),
        branches: network.branches.len(),
        generators: network.generators.len(),
        loads: network.loads.len(),
        fixed_shunts: network.fixed_shunts.len(),
        facts_devices: network.facts_devices.len(),
        hvdc_links: network.hvdc.links.len(),
    }
}

#[derive(Debug, Clone)]
struct AuxBusRow {
    substation_number: Option<u32>,
    latitude: Option<f64>,
    longitude: Option<f64>,
}

#[derive(Debug, Clone)]
struct AuxTables {
    substations: HashMap<u32, (Option<f64>, Option<f64>)>,
    buses: HashMap<u32, AuxBusRow>,
}

fn strip_aux_comment(line: &str) -> String {
    let mut in_quotes = false;
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut idx = 0usize;
    while idx < chars.len() {
        let ch = chars[idx];
        if ch == '"' {
            in_quotes = !in_quotes;
            out.push(ch);
            idx += 1;
            continue;
        }
        if !in_quotes && ch == '/' && idx + 1 < chars.len() && chars[idx + 1] == '/' {
            break;
        }
        out.push(ch);
        idx += 1;
    }
    out
}

fn split_aux_row(row: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in row.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
            }
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    fields.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        fields.push(current);
    }

    fields
}

fn parse_aux_fields(header: &str) -> Result<(String, Vec<String>)> {
    let data_start = header
        .find('(')
        .ok_or_else(|| anyhow!("AUX header missing opening parenthesis: {header}"))?;
    let section_and_rest = &header[data_start + 1..];
    let comma = section_and_rest
        .find(',')
        .ok_or_else(|| anyhow!("AUX header missing section delimiter: {header}"))?;
    let section_name = section_and_rest[..comma].trim().to_string();

    let open_bracket = header
        .find('[')
        .ok_or_else(|| anyhow!("AUX header missing field list: {header}"))?;
    let close_bracket = header[open_bracket + 1..]
        .find(']')
        .map(|idx| open_bracket + 1 + idx)
        .ok_or_else(|| anyhow!("AUX header missing closing bracket: {header}"))?;
    let fields = header[open_bracket + 1..close_bracket]
        .split(',')
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    Ok((section_name, fields))
}

fn parse_aux_u32(token: Option<&String>) -> Option<u32> {
    token.and_then(|value| value.parse::<u32>().ok())
}

fn parse_aux_coordinate(token: Option<&String>) -> Option<f64> {
    token
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && value.abs() > 1e-10)
}

fn parse_aux_tables(aux_path: &std::path::Path) -> Result<AuxTables> {
    let payload = std::fs::read_to_string(aux_path)
        .with_context(|| format!("failed to read AUX file {}", aux_path.display()))?;

    let mut substations = HashMap::<u32, (Option<f64>, Option<f64>)>::new();
    let mut buses = HashMap::<u32, AuxBusRow>::new();

    let lines: Vec<&str> = payload.lines().collect();
    let mut idx = 0usize;
    while idx < lines.len() {
        let cleaned = strip_aux_comment(lines[idx]);
        let trimmed = cleaned.trim();
        if !trimmed.to_ascii_lowercase().starts_with("data (") {
            idx += 1;
            continue;
        }

        let mut header = trimmed.to_string();
        while !header.contains(']') {
            idx += 1;
            if idx >= lines.len() {
                bail!(
                    "unterminated AUX header while parsing {}",
                    aux_path.display()
                );
            }
            let next = strip_aux_comment(lines[idx]);
            if !next.trim().is_empty() {
                header.push(' ');
                header.push_str(next.trim());
            }
        }

        let (section_name, fields) = parse_aux_fields(&header)?;
        let section_name_lc = section_name.to_ascii_lowercase();

        while idx < lines.len() && !strip_aux_comment(lines[idx]).contains('{') {
            idx += 1;
        }
        if idx >= lines.len() {
            bail!(
                "AUX section {} missing opening brace in {}",
                section_name,
                aux_path.display()
            );
        }

        idx += 1;
        while idx < lines.len() {
            let row = strip_aux_comment(lines[idx]);
            let trimmed_row = row.trim();
            if trimmed_row.is_empty() {
                idx += 1;
                continue;
            }
            if trimmed_row.starts_with('}') {
                break;
            }

            let tokens = split_aux_row(trimmed_row);
            if tokens.is_empty() {
                idx += 1;
                continue;
            }

            if section_name_lc == "substation" {
                let sub_num = parse_aux_u32(tokens.first())
                    .ok_or_else(|| anyhow!("failed to parse AUX Substation row: {trimmed_row}"))?;
                let latitude = parse_aux_coordinate(tokens.get(3));
                let longitude = parse_aux_coordinate(tokens.get(4));
                substations.insert(sub_num, (latitude, longitude));
            } else if section_name_lc == "bus" {
                let bus_number = parse_aux_u32(tokens.first())
                    .ok_or_else(|| anyhow!("failed to parse AUX Bus row: {trimmed_row}"))?;
                let sub_num_idx = fields
                    .iter()
                    .position(|field| field.eq_ignore_ascii_case("SubNum"));
                let latitude_indices: Vec<usize> = fields
                    .iter()
                    .enumerate()
                    .filter_map(|(field_idx, field)| {
                        field
                            .to_ascii_lowercase()
                            .starts_with("latitude")
                            .then_some(field_idx)
                    })
                    .collect();
                let longitude_indices: Vec<usize> = fields
                    .iter()
                    .enumerate()
                    .filter_map(|(field_idx, field)| {
                        field
                            .to_ascii_lowercase()
                            .starts_with("longitude")
                            .then_some(field_idx)
                    })
                    .collect();

                let latitude = latitude_indices
                    .iter()
                    .find_map(|&field_idx| parse_aux_coordinate(tokens.get(field_idx)));
                let longitude = longitude_indices
                    .iter()
                    .find_map(|&field_idx| parse_aux_coordinate(tokens.get(field_idx)));
                let substation_number =
                    sub_num_idx.and_then(|field_idx| parse_aux_u32(tokens.get(field_idx)));

                buses.insert(
                    bus_number,
                    AuxBusRow {
                        substation_number,
                        latitude,
                        longitude,
                    },
                );
            }

            idx += 1;
        }

        idx += 1;
    }

    Ok(AuxTables { substations, buses })
}

fn merge_aux_backfill(
    network: &mut Network,
    aux_tables: &AuxTables,
    aux_path: &std::path::Path,
) -> AuxBackfillSummary {
    let mut matched_buses = 0usize;
    let mut buses_with_direct_aux_coordinates = 0usize;
    let mut buses_with_substation_fallback_coordinates = 0usize;

    for bus in &mut network.buses {
        let Some(aux_bus) = aux_tables.buses.get(&bus.number) else {
            continue;
        };
        matched_buses += 1;

        let direct_lat = aux_bus.latitude;
        let direct_lon = aux_bus.longitude;
        if direct_lat.is_some() || direct_lon.is_some() {
            bus.latitude = direct_lat;
            bus.longitude = direct_lon;
            buses_with_direct_aux_coordinates += 1;
            continue;
        }

        let fallback = aux_bus
            .substation_number
            .and_then(|sub_num| aux_tables.substations.get(&sub_num).copied());
        if let Some((latitude, longitude)) = fallback
            && (latitude.is_some() || longitude.is_some())
        {
            bus.latitude = latitude;
            bus.longitude = longitude;
            buses_with_substation_fallback_coordinates += 1;
        }
    }

    let buses_with_coordinates_after_merge = network
        .buses
        .iter()
        .filter(|bus| bus.latitude.is_some() || bus.longitude.is_some())
        .count();
    let substations_with_coordinates = aux_tables
        .substations
        .values()
        .filter(|(latitude, longitude)| latitude.is_some() || longitude.is_some())
        .count();

    AuxBackfillSummary {
        aux_path: aux_path.display().to_string(),
        matched_buses,
        buses_with_direct_aux_coordinates,
        buses_with_substation_fallback_coordinates,
        buses_with_coordinates_after_merge,
        substations_with_coordinates,
    }
}

fn generator_indices_by_bus(network: &Network) -> BTreeMap<u32, Vec<usize>> {
    let mut grouped = BTreeMap::<u32, Vec<usize>>::new();
    for (idx, generator) in network.generators.iter().enumerate() {
        grouped.entry(generator.bus).or_default().push(idx);
    }
    grouped
}

fn generator_match_score(
    raw_generator: &Generator,
    mat_generator: &Generator,
    raw_ordinal: usize,
    mat_ordinal: usize,
) -> f64 {
    let mut score = 0.0;
    score += (raw_generator.p - mat_generator.p).abs();
    score += 0.5 * (raw_generator.q - mat_generator.q).abs();
    score += 0.01 * (raw_generator.machine_base_mva - mat_generator.machine_base_mva).abs();
    score += 0.001 * (raw_generator.voltage_setpoint_pu - mat_generator.voltage_setpoint_pu).abs();
    if raw_generator.in_service != mat_generator.in_service {
        score += 1000.0;
    }
    score + 0.0001 * (raw_ordinal as f64 - mat_ordinal as f64).abs()
}

fn best_bus_group_assignment(
    raw_indices: &[usize],
    mat_indices: &[usize],
    raw_network: &Network,
    mat_network: &Network,
) -> Result<Vec<(usize, usize)>> {
    if raw_indices.len() != mat_indices.len() {
        bail!(
            "generator multiplicity mismatch on bus {}: RAW has {}, MATPOWER has {}",
            raw_network.generators[raw_indices[0]].bus,
            raw_indices.len(),
            mat_indices.len()
        );
    }
    if raw_indices.is_empty() {
        return Ok(Vec::new());
    }

    let n = raw_indices.len();
    if n > 16 {
        bail!(
            "bus {} has {} generators; refresh helper only supports exact assignment up to 16 per bus",
            raw_network.generators[raw_indices[0]].bus,
            n
        );
    }

    let state_count = 1usize << n;
    let mut dp = vec![f64::INFINITY; state_count];
    let mut parent: Vec<Option<(usize, usize)>> = vec![None; state_count];
    dp[0] = 0.0;

    for mask in 0..state_count {
        let raw_pos = mask.count_ones() as usize;
        if raw_pos >= n || !dp[mask].is_finite() {
            continue;
        }
        let raw_idx = raw_indices[raw_pos];
        for (mat_pos, &mat_idx) in mat_indices.iter().enumerate().take(n) {
            if (mask & (1usize << mat_pos)) != 0 {
                continue;
            }
            let pair_score = generator_match_score(
                &raw_network.generators[raw_idx],
                &mat_network.generators[mat_idx],
                raw_pos,
                mat_pos,
            );
            let next_mask = mask | (1usize << mat_pos);
            let next_score = dp[mask] + pair_score;
            if next_score < dp[next_mask] {
                dp[next_mask] = next_score;
                parent[next_mask] = Some((mask, mat_pos));
            }
        }
    }

    let full_mask = state_count - 1;
    if !dp[full_mask].is_finite() {
        bail!(
            "failed to build a generator assignment for bus {}",
            raw_network.generators[raw_indices[0]].bus
        );
    }

    let mut assignments = vec![(0usize, 0usize); n];
    let mut mask = full_mask;
    while mask != 0 {
        let raw_pos = mask.count_ones() as usize - 1;
        let Some((prev_mask, mat_pos)) = parent[mask] else {
            bail!(
                "assignment backtracking failed for bus {}",
                raw_network.generators[raw_indices[0]].bus
            );
        };
        assignments[raw_pos] = (raw_indices[raw_pos], mat_indices[mat_pos]);
        mask = prev_mask;
    }
    Ok(assignments)
}

fn merge_fuel_fields(raw_generator: &mut Generator, mat_generator: &Generator) -> bool {
    let Some(mat_fuel) = mat_generator.fuel.as_ref() else {
        return false;
    };
    let mut changed = false;
    let raw_fuel = raw_generator.fuel.get_or_insert_with(FuelParams::default);
    if mat_fuel.fuel_type.is_some() {
        raw_fuel.fuel_type = mat_fuel.fuel_type.clone();
        changed = true;
    }
    if mat_fuel.heat_rate_btu_mwh.is_some() {
        raw_fuel.heat_rate_btu_mwh = mat_fuel.heat_rate_btu_mwh;
        changed = true;
    }
    if mat_fuel.primary_fuel.is_some() {
        raw_fuel.primary_fuel = mat_fuel.primary_fuel.clone();
        changed = true;
    }
    if mat_fuel.backup_fuel.is_some() {
        raw_fuel.backup_fuel = mat_fuel.backup_fuel.clone();
        changed = true;
    }
    if mat_fuel.fuel_switch_time_min.is_some() {
        raw_fuel.fuel_switch_time_min = mat_fuel.fuel_switch_time_min;
        changed = true;
    }
    if mat_fuel.on_backup_fuel {
        raw_fuel.on_backup_fuel = true;
        changed = true;
    }
    if mat_fuel.emission_rates.co2 != 0.0
        || mat_fuel.emission_rates.nox != 0.0
        || mat_fuel.emission_rates.so2 != 0.0
        || mat_fuel.emission_rates.pm25 != 0.0
    {
        raw_fuel.emission_rates = mat_fuel.emission_rates.clone();
        changed = true;
    }
    changed
}

fn merge_matpower_backfill(
    raw_network: &mut Network,
    matpower_network: &Network,
    matpower_path: &std::path::Path,
) -> Result<MatpowerBackfillSummary> {
    if raw_network.generators.len() != matpower_network.generators.len() {
        bail!(
            "generator count mismatch between RAW ({}) and MATPOWER ({})",
            raw_network.generators.len(),
            matpower_network.generators.len()
        );
    }

    let raw_by_bus = generator_indices_by_bus(raw_network);
    let mat_by_bus = generator_indices_by_bus(matpower_network);
    if raw_by_bus.len() != mat_by_bus.len() || raw_by_bus.keys().ne(mat_by_bus.keys()) {
        let raw_buses: Vec<u32> = raw_by_bus.keys().copied().collect();
        let mat_buses: Vec<u32> = mat_by_bus.keys().copied().collect();
        bail!(
            "generator bus layout mismatch between RAW and MATPOWER\nRAW buses: {:?}\nMATPOWER buses: {:?}",
            raw_buses,
            mat_buses
        );
    }

    let mut assignments = Vec::<(usize, usize)>::with_capacity(raw_network.generators.len());
    for (bus, raw_indices) in &raw_by_bus {
        let mat_indices = mat_by_bus
            .get(bus)
            .expect("bus key existence was validated above");
        assignments.extend(best_bus_group_assignment(
            raw_indices,
            mat_indices,
            raw_network,
            matpower_network,
        )?);
    }

    assignments.sort_by_key(|(raw_idx, _)| *raw_idx);

    let mut summary = MatpowerBackfillSummary {
        matpower_path: matpower_path.display().to_string(),
        matched_generators: assignments.len(),
        generators_with_cost: 0,
        generators_with_agc: 0,
        generators_with_ramping: 0,
        generators_with_reactive_capability: 0,
        generators_with_fuel_type: 0,
        generators_with_electrical_class: 0,
        generators_with_technology: 0,
        generators_with_source_technology_code: 0,
    };

    for (raw_idx, mat_idx) in assignments {
        let mat_generator = &matpower_network.generators[mat_idx];
        let raw_generator = &mut raw_network.generators[raw_idx];

        if mat_generator.cost.is_some() {
            raw_generator.cost = mat_generator.cost.clone();
            summary.generators_with_cost += 1;
        }
        if mat_generator.agc_participation_factor.is_some() {
            raw_generator.agc_participation_factor = mat_generator.agc_participation_factor;
            summary.generators_with_agc += 1;
        }
        if mat_generator.ramping.is_some() {
            raw_generator.ramping = mat_generator.ramping.clone();
            summary.generators_with_ramping += 1;
        }
        if mat_generator.reactive_capability.is_some() {
            raw_generator.reactive_capability = mat_generator.reactive_capability.clone();
            summary.generators_with_reactive_capability += 1;
        }
        if merge_fuel_fields(raw_generator, mat_generator)
            && raw_generator
                .fuel
                .as_ref()
                .and_then(|fuel| fuel.fuel_type.as_ref())
                .is_some()
        {
            summary.generators_with_fuel_type += 1;
        }
        if mat_generator.gen_type != GenType::Unknown {
            raw_generator.gen_type = mat_generator.gen_type;
            summary.generators_with_electrical_class += 1;
        }
        if mat_generator.technology.is_some() {
            raw_generator.technology = mat_generator.technology;
            summary.generators_with_technology += 1;
        }
        if mat_generator.source_technology_code.is_some() {
            raw_generator.source_technology_code = mat_generator.source_technology_code.clone();
            summary.generators_with_source_technology_code += 1;
        }
    }

    Ok(summary)
}

fn regulating_target_counts(network: &Network) -> HashMap<u32, usize> {
    network
        .generators
        .iter()
        .filter(|generator| generator.in_service && generator.voltage_regulated)
        .fold(HashMap::new(), |mut counts, generator| {
            let target_bus = generator.reg_bus.unwrap_or(generator.bus);
            *counts.entry(target_bus).or_insert(0) += 1;
            counts
        })
}

fn component_adjacency(network: &Network) -> Vec<Vec<usize>> {
    let bus_index = network.bus_index_map();
    let mut adjacency = vec![Vec::new(); network.buses.len()];
    for branch in network.branches.iter().filter(|branch| branch.in_service) {
        let Some(&from_idx) = bus_index.get(&branch.from_bus) else {
            continue;
        };
        let Some(&to_idx) = bus_index.get(&branch.to_bus) else {
            continue;
        };
        adjacency[from_idx].push(to_idx);
        adjacency[to_idx].push(from_idx);
    }
    adjacency
}

fn choose_slack_bus(
    network: &Network,
    component: &[usize],
    previous_slacks: &[u32],
    regulating_targets: &HashMap<u32, usize>,
) -> Result<u32> {
    let mut candidates: Vec<(bool, bool, u32)> = component
        .iter()
        .filter_map(|&idx| {
            let bus = &network.buses[idx];
            let reg_count = regulating_targets.get(&bus.number).copied().unwrap_or(0);
            if reg_count == 0 {
                return None;
            }
            Some((
                previous_slacks.contains(&bus.number),
                bus.bus_type == BusType::PV,
                bus.number,
            ))
        })
        .collect();

    if candidates.is_empty() {
        let buses: Vec<u32> = component
            .iter()
            .map(|&idx| network.buses[idx].number)
            .collect();
        bail!(
            "no regulating-bus candidate found when repairing slack placement for component {:?}",
            buses
        );
    }

    candidates.sort_by_key(|(was_slack, was_pv, bus_number)| (!*was_slack, !*was_pv, *bus_number));
    Ok(candidates[0].2)
}

fn repair_component_slack_assignments(network: &mut Network) -> Result<Vec<ComponentSlackRepair>> {
    let regulating_targets = regulating_target_counts(network);
    let adjacency = component_adjacency(network);
    let mut visited = vec![false; network.buses.len()];
    let mut repairs = Vec::new();

    for start_idx in 0..network.buses.len() {
        if visited[start_idx] {
            continue;
        }
        if network.buses[start_idx].bus_type == BusType::Isolated {
            visited[start_idx] = true;
            continue;
        }

        let mut stack = vec![start_idx];
        let mut component = Vec::new();
        while let Some(idx) = stack.pop() {
            if visited[idx] {
                continue;
            }
            visited[idx] = true;
            if network.buses[idx].bus_type == BusType::Isolated {
                continue;
            }
            component.push(idx);
            for &next in &adjacency[idx] {
                if !visited[next] {
                    stack.push(next);
                }
            }
        }

        if component.is_empty() {
            continue;
        }

        let previous_slacks: Vec<u32> = component
            .iter()
            .filter_map(|&idx| {
                (network.buses[idx].bus_type == BusType::Slack).then_some(network.buses[idx].number)
            })
            .collect();
        let valid_previous_slacks: Vec<u32> = previous_slacks
            .iter()
            .copied()
            .filter(|bus| regulating_targets.get(bus).copied().unwrap_or(0) > 0)
            .collect();

        let chosen_slack_bus = if valid_previous_slacks.len() == 1 {
            valid_previous_slacks[0]
        } else {
            choose_slack_bus(network, &component, &previous_slacks, &regulating_targets)?
        };

        let mut demoted_stale_slacks = Vec::new();
        let promoted_to_slack = !previous_slacks.contains(&chosen_slack_bus);

        for &idx in &component {
            let bus_number = network.buses[idx].number;
            let has_regulator = regulating_targets.get(&bus_number).copied().unwrap_or(0) > 0;
            let new_type = if bus_number == chosen_slack_bus {
                BusType::Slack
            } else if has_regulator {
                BusType::PV
            } else {
                BusType::PQ
            };

            if network.buses[idx].bus_type == BusType::Slack && bus_number != chosen_slack_bus {
                demoted_stale_slacks.push(bus_number);
            }
            network.buses[idx].bus_type = new_type;
        }

        if previous_slacks != vec![chosen_slack_bus] || !demoted_stale_slacks.is_empty() {
            repairs.push(ComponentSlackRepair {
                component_size: component.len(),
                previous_slack_buses: previous_slacks,
                chosen_slack_bus,
                promoted_to_slack,
                demoted_stale_slacks,
            });
        }
    }

    Ok(repairs)
}

fn main() -> Result<()> {
    let args = Args::parse();

    let mut network = surge_io::psse::raw::load(&args.raw_path)
        .with_context(|| format!("failed to load RAW case {}", args.raw_path.display()))?;
    let aux_backfill = if let Some(aux_path) = args.aux_path.as_ref() {
        let aux_tables = parse_aux_tables(aux_path)?;
        Some(merge_aux_backfill(&mut network, &aux_tables, aux_path))
    } else {
        None
    };
    let matpower_backfill = if let Some(matpower_path) = args.matpower_path.as_ref() {
        let matpower_network = surge_io::matpower::load(matpower_path)
            .with_context(|| format!("failed to load MATPOWER case {}", matpower_path.display()))?;
        Some(merge_matpower_backfill(
            &mut network,
            &matpower_network,
            matpower_path,
        )?)
    } else {
        None
    };
    network.canonicalize_runtime_identities();
    let slack_repairs = repair_component_slack_assignments(&mut network)?;
    network.validate().map_err(|error| anyhow!(error))?;

    surge_io::save(&network, &args.output_path)
        .with_context(|| format!("failed to save {}", args.output_path.display()))?;

    let summary = RefreshSummary {
        raw_path: args.raw_path.display().to_string(),
        output_path: args.output_path.display().to_string(),
        counts: network_counts(&network),
        slack_repairs,
        aux_backfill,
        matpower_backfill,
    };

    if let Some(summary_path) = args.summary_path {
        if let Some(parent) = summary_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&summary_path, serde_json::to_vec_pretty(&summary)?)
            .with_context(|| format!("failed to write {}", summary_path.display()))?;
    }

    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}
