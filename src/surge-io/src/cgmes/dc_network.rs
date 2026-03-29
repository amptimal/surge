// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES DC network builder.

use std::collections::HashMap;

use surge_network::Network;

use super::indices::CgmesIndices;
use super::types::ObjMap;

/// Build DC network structs from CGMES DC topology.
///
/// Returns the set of converter mRIDs that were successfully mapped to DC network structs.
/// These converters should NOT also be modeled as PQ injections (to avoid double-counting).
///
/// Only populates when the CGMES data has actual DC topology (DCNode/DCTopologicalNode objects
/// with VsConverter/CsConverter having resolvable DC terminals).
pub(crate) fn build_dc_network(
    objects: &ObjMap,
    idx: &CgmesIndices,
    network: &mut Network,
    bus_num_to_idx: &HashMap<u32, usize>,
) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    use surge_network::network::{
        DcBranch, DcBus, DcConverter, DcConverterStation, LccDcConverter, LccDcConverterRole,
    };

    let mut handled_convs: HashSet<String> = HashSet::new();

    // Collect all DCNode/DCTopologicalNode mRIDs that have at least one converter attached.
    let dc_node_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| matches!(o.class.as_str(), "DCNode" | "DCTopologicalNode"))
        .map(|(id, _)| id.clone())
        .collect();

    if dc_node_ids.is_empty() {
        return handled_convs;
    }

    // Check if any converter has a resolvable DC terminal.
    let conv_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| matches!(o.class.as_str(), "VsConverter" | "CsConverter"))
        .filter(|(id, _)| idx.conv_to_dcnode.contains_key(id.as_str()))
        .map(|(id, _)| id.clone())
        .collect();

    if conv_ids.is_empty() {
        return handled_convs;
    }

    // Assign DC bus numbers (1-based).
    let mut dcnode_bus_id: HashMap<String, u32> = HashMap::new();
    for (i, node_id) in dc_node_ids.iter().enumerate() {
        dcnode_bus_id.insert(node_id.clone(), (i + 1) as u32);
    }

    // DC grid assignment via BFS on DCLineSegment/HvdcLine connectivity.
    let mut dcnode_grid: HashMap<String, u32> = HashMap::new();
    let mut grid_counter = 0u32;
    for node_id in &dc_node_ids {
        if dcnode_grid.contains_key(node_id) {
            continue;
        }
        grid_counter += 1;
        // BFS from this node.
        let mut queue = vec![node_id.clone()];
        dcnode_grid.insert(node_id.clone(), grid_counter);
        while let Some(current) = queue.pop() {
            // Find all equipment connected to this node.
            if let Some(eq_list) = idx.dcnode_to_eq.get(current.as_str()) {
                for (eq_id, eq_class) in eq_list {
                    if !matches!(eq_class.as_str(), "DCLineSegment" | "HvdcLine") {
                        continue;
                    }
                    // Find the other DCNode connected to this equipment.
                    for other_node in &dc_node_ids {
                        if other_node == &current || dcnode_grid.contains_key(other_node) {
                            continue;
                        }
                        if let Some(other_eq_list) = idx.dcnode_to_eq.get(other_node.as_str())
                            && other_eq_list.iter().any(|(eid, _)| eid == eq_id)
                        {
                            dcnode_grid.insert(other_node.clone(), grid_counter);
                            queue.push(other_node.clone());
                        }
                    }
                }
            }
        }
    }

    // Resolve base_kv_dc: prefer HvdcLine.ratedUdc, then converter ratedUdc, then 320 kV default.
    let resolve_base_kv_dc = |node_id: &str| -> f64 {
        // Try HvdcLine.ratedUdc via dcnode → HvdcLine mapping.
        if let Some(eq_list) = idx.dcnode_to_eq.get(node_id) {
            for (eq_id, eq_class) in eq_list {
                if eq_class == "HvdcLine"
                    && let Some(&(_, _, Some(udc))) = idx.hvdc_line_params.get(eq_id.as_str())
                    && udc > 0.0
                {
                    return udc;
                }
            }
        }
        // Try converter ratedUdc.
        for (conv_id, dcnode_id) in &idx.conv_to_dcnode {
            if dcnode_id == node_id
                && let Some(conv) = objects.get(conv_id.as_str())
                && let Some(udc) = super::indices::parse_optional_f64(conv, "ratedUdc")
                && udc > 0.0
            {
                return udc;
            }
        }
        320.0 // default
    };

    // Build DcBus entries.
    for node_id in &dc_node_ids {
        let bus_id = dcnode_bus_id[node_id];
        let grid_id = dcnode_grid.get(node_id).copied().unwrap_or(1);
        let base_kv_dc = resolve_base_kv_dc(node_id);
        network
            .hvdc
            .ensure_dc_grid(grid_id, None)
            .buses
            .push(DcBus {
                bus_id,
                p_dc_mw: 0.0,
                v_dc_pu: 1.0,
                base_kv_dc,
                v_dc_max: 1.1,
                v_dc_min: 0.9,
                cost: 0.0,
                g_shunt_siemens: 0.0,
                r_ground_ohm: 0.0,
            });
    }

    // Build DcConverterStation entries.
    let base_mva = network.base_mva;
    for conv_id in &conv_ids {
        let conv = &objects[conv_id];
        let dcnode_id = match idx.conv_to_dcnode.get(conv_id.as_str()) {
            Some(n) => n,
            None => continue,
        };
        let dc_bus = match dcnode_bus_id.get(dcnode_id.as_str()) {
            Some(&id) => id,
            None => continue,
        };

        // Resolve AC bus number.
        let ac_bus = match idx.terminals(conv_id).iter().find_map(|tid| {
            let tn = idx.terminal_tn(objects, tid)?;
            idx.tn_bus(tn)
        }) {
            Some(n) => n,
            None => continue,
        };

        // Check AC bus exists.
        if !bus_num_to_idx.contains_key(&ac_bus) {
            continue;
        }

        let is_lcc = conv.class == "CsConverter";
        let grid_id = dcnode_grid.get(dcnode_id.as_str()).copied().unwrap_or(1);

        // Power setpoints.
        let hvdc_p_fallback = idx
            .conv_hvdc
            .get(conv_id.as_str())
            .and_then(|hvdc_id| idx.hvdc_line_params.get(hvdc_id.as_str()))
            .and_then(|&(p, _, _)| p);
        let p_g = super::indices::parse_optional_f64(conv, "p")
            .or_else(|| super::indices::parse_optional_f64(conv, "targetPpcc"))
            .or(hvdc_p_fallback)
            .unwrap_or(0.0);
        let q_g = super::indices::parse_optional_f64(conv, "q")
            .or_else(|| super::indices::parse_optional_f64(conv, "targetQpcc"))
            .unwrap_or(0.0);

        // Voltage target.
        let base_kv_ac = bus_num_to_idx
            .get(&ac_bus)
            .and_then(|&i| network.buses.get(i))
            .map(|b| b.base_kv)
            .unwrap_or(1.0)
            .max(1e-3);
        let v_tar = super::indices::parse_optional_f64(conv, "targetUpcc")
            .map(|u| u / base_kv_ac)
            .unwrap_or(1.0);

        // Ratings.
        let rating_mva = super::indices::parse_optional_f64(conv, "ratedS")
            .unwrap_or(0.0)
            .max(super::indices::parse_optional_f64(conv, "ratedPdc").unwrap_or(0.0));
        let pac_bound = if rating_mva > 0.0 { rating_mva } else { 9999.0 };
        let qac_max = conv.parse_f64("maxQ").unwrap_or(pac_bound);
        let qac_min = conv.parse_f64("minQ").unwrap_or(-pac_bound);

        // Loss parameters.
        // CGMES: idleLoss (MW), switchingLoss (relative), resistiveLoss (relative).
        // MatACDC convention: LossA (MW), LossB (kV), LossC (ohms).
        let rated_udc = super::indices::parse_optional_f64(conv, "ratedUdc")
            .filter(|v| *v > 0.0)
            .unwrap_or_else(|| resolve_base_kv_dc(dcnode_id));
        let base_s = if rating_mva > 0.0 {
            rating_mva
        } else {
            base_mva
        };

        let loss_a = super::indices::parse_optional_f64(conv, "idleLoss").unwrap_or(1.103);
        let loss_b = super::indices::parse_optional_f64(conv, "switchingLoss")
            .map(|sl| sl * base_s / rated_udc.max(1.0))
            .unwrap_or(0.887);
        let loss_c = super::indices::parse_optional_f64(conv, "resistiveLoss")
            .map(|rl| rl * rated_udc * rated_udc / base_s.max(1.0))
            .unwrap_or(2.885);

        // Droop.
        let droop = super::indices::parse_optional_f64(conv, "droop").unwrap_or(0.0);

        // DC voltage setpoint.
        let v_dc_set = super::indices::parse_optional_f64(conv, "targetUdc")
            .map(|u| u / rated_udc.max(1.0))
            .unwrap_or(1.0);

        // AC-side type.
        let q_ctrl = conv.get_ref("qPccControl").unwrap_or("");
        let is_voltage_pcc = q_ctrl.ends_with("voltagePcc") || q_ctrl.ends_with(".voltagePcc");
        let control_type_ac: u32 = if is_voltage_pcc { 2 } else { 1 };

        // Current limit.
        let i_max = super::indices::parse_optional_f64(conv, "maxValveCurrent").unwrap_or(0.0);

        if is_lcc {
            network
                .hvdc
                .ensure_dc_grid(grid_id, None)
                .converters
                .push(DcConverter::Lcc(LccDcConverter {
                    id: String::new(),
                    dc_bus,
                    ac_bus,
                    n_bridges: 1,
                    alpha_max_deg: 90.0,
                    alpha_min_deg: 5.0,
                    gamma_min_deg: 15.0,
                    commutation_resistance_ohm: 0.0,
                    commutation_reactance_ohm: 0.0,
                    base_voltage_kv: base_kv_ac,
                    turns_ratio: 1.0,
                    tap_ratio: 1.0,
                    tap_max: 1.1,
                    tap_min: 0.9,
                    tap_step: 0.00625,
                    scheduled_setpoint: p_g,
                    power_share_percent: 0.0,
                    current_margin_percent: 0.0,
                    role: if p_g >= 0.0 {
                        LccDcConverterRole::Rectifier
                    } else {
                        LccDcConverterRole::Inverter
                    },
                    in_service: true,
                }));
        } else {
            network
                .hvdc
                .ensure_dc_grid(grid_id, None)
                .converters
                .push(DcConverter::Vsc(DcConverterStation {
                    id: String::new(),
                    dc_bus,
                    ac_bus,
                    control_type_dc: 1, // placeholder — assigned below per grid
                    control_type_ac,
                    active_power_mw: p_g,
                    reactive_power_mvar: q_g,
                    is_lcc: false,
                    voltage_setpoint_pu: v_tar,
                    transformer_r_pu: 0.0,
                    transformer_x_pu: 0.0,
                    transformer: false,
                    tap_ratio: 1.0,
                    filter_susceptance_pu: 0.0,
                    filter: false,
                    reactor_r_pu: 0.0,
                    reactor_x_pu: 0.0,
                    reactor: false,
                    base_kv_ac,
                    voltage_max_pu: 1.1,
                    voltage_min_pu: 0.9,
                    current_max_pu: i_max,
                    status: true,
                    loss_constant_mw: loss_a,
                    loss_linear: loss_b,
                    loss_quadratic_rectifier: loss_c,
                    loss_quadratic_inverter: loss_c,
                    droop,
                    power_dc_setpoint_mw: p_g,
                    voltage_dc_setpoint_pu: v_dc_set,
                    active_power_ac_max_mw: pac_bound,
                    active_power_ac_min_mw: -pac_bound,
                    reactive_power_ac_max_mvar: qac_max,
                    reactive_power_ac_min_mvar: qac_min,
                }));
        }
        handled_convs.insert(conv_id.clone());
    }

    // Assign control_type_dc: one Vdc-slack (type_dc=2) per grid, rest P-control (1) or droop (3).
    for dc_grid in &mut network.hvdc.dc_grids {
        let mut largest_vsc: Option<(usize, f64)> = None;
        for (index, converter) in dc_grid.converters.iter().enumerate() {
            let Some(vsc) = converter.as_vsc() else {
                continue;
            };
            let rating_mva = vsc
                .base_kv_ac
                .max(0.0)
                .max(vsc.active_power_ac_max_mw.abs())
                .max(vsc.active_power_ac_min_mw.abs())
                .max(vsc.reactive_power_ac_max_mvar.abs())
                .max(vsc.reactive_power_ac_min_mvar.abs());
            if largest_vsc.is_none_or(|(_, current_rating)| rating_mva > current_rating) {
                largest_vsc = Some((index, rating_mva));
            }
        }
        for (index, converter) in dc_grid.converters.iter_mut().enumerate() {
            let Some(vsc) = converter.as_vsc_mut() else {
                continue;
            };
            if largest_vsc.map(|(largest_index, _)| largest_index) == Some(index) {
                vsc.control_type_dc = 2;
                vsc.voltage_dc_setpoint_pu = 1.0;
            } else if vsc.droop.abs() > 1e-10 {
                vsc.control_type_dc = 3;
            } else {
                vsc.control_type_dc = 1;
            }
        }
    }

    // Build DcBranch entries from DCLineSegment objects.
    let mut seen_branches: HashSet<(u32, u32)> = HashSet::new();
    for (eq_id, eq_obj) in objects.iter().filter(|(_, o)| o.class == "DCLineSegment") {
        // Find the two DCNodes this segment connects to via DCTerminal.
        let mut endpoints: Vec<u32> = Vec::new();
        for node_id in &dc_node_ids {
            if let Some(eq_list) = idx.dcnode_to_eq.get(node_id.as_str())
                && eq_list.iter().any(|(eid, _)| eid == eq_id)
                && let Some(&bus_id) = dcnode_bus_id.get(node_id.as_str())
            {
                endpoints.push(bus_id);
            }
        }
        if endpoints.len() >= 2 {
            let (from, to) = (
                endpoints[0].min(endpoints[1]),
                endpoints[0].max(endpoints[1]),
            );
            if !seen_branches.insert((from, to)) {
                continue; // duplicate
            }
            let r_ohm = super::indices::parse_optional_f64(eq_obj, "resistance").unwrap_or(0.01);
            let l_mh =
                super::indices::parse_optional_f64(eq_obj, "inductance").unwrap_or(0.0) * 1000.0;
            let c_uf =
                super::indices::parse_optional_f64(eq_obj, "capacitance").unwrap_or(0.0) * 1e6;
            let rate_a =
                super::indices::parse_optional_f64(eq_obj, "ratedPower").unwrap_or(0.0) / 1e6;
            let rate_a = if rate_a > 0.0 { rate_a } else { 9999.0 };
            if let Some(grid) = network.hvdc.find_dc_grid_by_bus_mut(from) {
                let branch_id = format!("dc_grid_{}_branch_{}", grid.id, grid.branches.len() + 1);
                grid.branches.push(DcBranch {
                    id: branch_id,
                    from_bus: from,
                    to_bus: to,
                    r_ohm,
                    l_mh,
                    c_uf,
                    rating_a_mva: rate_a,
                    rating_b_mva: 0.0,
                    rating_c_mva: 0.0,
                    status: true,
                });
            }
        }
    }

    // HvdcLine fallback: create synthetic DcBranch when no DCLineSegment exists between
    // converter DC nodes but an HvdcLine connects them.
    for (hvdc_id, hvdc_params) in &idx.hvdc_line_params {
        let Some(r_ohm) = hvdc_params.1 else {
            continue;
        };
        // Find all DCNodes connected to this HvdcLine.
        let mut hvdc_nodes: Vec<u32> = Vec::new();
        for (node_id, eq_list) in &idx.dcnode_to_eq {
            if eq_list
                .iter()
                .any(|(eid, ec)| eid == hvdc_id && ec == "HvdcLine")
                && let Some(&bus_id) = dcnode_bus_id.get(node_id.as_str())
            {
                hvdc_nodes.push(bus_id);
            }
        }
        if hvdc_nodes.len() >= 2 {
            let (from, to) = (
                hvdc_nodes[0].min(hvdc_nodes[1]),
                hvdc_nodes[0].max(hvdc_nodes[1]),
            );
            if seen_branches.contains(&(from, to)) {
                continue; // already have a DCLineSegment branch
            }
            seen_branches.insert((from, to));
            let r = if r_ohm > 1e-10 { r_ohm } else { 0.01 };
            if let Some(grid) = network.hvdc.find_dc_grid_by_bus_mut(from) {
                let branch_id = format!("dc_grid_{}_branch_{}", grid.id, grid.branches.len() + 1);
                grid.branches.push(DcBranch {
                    id: branch_id,
                    from_bus: from,
                    to_bus: to,
                    r_ohm: r,
                    l_mh: 0.0,
                    c_uf: 0.0,
                    rating_a_mva: 9999.0,
                    rating_b_mva: 0.0,
                    rating_c_mva: 0.0,
                    status: true,
                });
            }
        }
    }

    // --- DCSeriesDevice → add series resistance/inductance to parent DcBranch ---
    //
    // CGMES: DCSeriesDevice is a series reactor/filter on a DC cable.
    // Its resistance adds to the cable's I²R losses.  Resolve which DcBranch
    // it belongs to via DCTerminal connectivity (same endpoints as a DCLineSegment).
    let mut n_series_devices = 0u32;
    for (sd_id, sd_obj) in objects.iter().filter(|(_, o)| o.class == "DCSeriesDevice") {
        let r_add = sd_obj.parse_f64("resistance").unwrap_or(0.0);
        let l_add_h = sd_obj.parse_f64("inductance").unwrap_or(0.0);
        if r_add.abs() < 1e-12 && l_add_h.abs() < 1e-12 {
            continue;
        }
        // Find the two DCNodes this device connects to via DCTerminal.
        let mut endpoints: Vec<u32> = Vec::new();
        for node_id in &dc_node_ids {
            if let Some(eq_list) = idx.dcnode_to_eq.get(node_id.as_str())
                && eq_list.iter().any(|(eid, _)| eid == sd_id)
                && let Some(&bus_id) = dcnode_bus_id.get(node_id.as_str())
            {
                endpoints.push(bus_id);
            }
        }
        if endpoints.len() >= 2 {
            let (from, to) = (
                endpoints[0].min(endpoints[1]),
                endpoints[0].max(endpoints[1]),
            );
            // Find the matching DcBranch and add the series impedance.
            for br in network.hvdc.dc_branches_mut() {
                let br_pair = (br.from_bus.min(br.to_bus), br.from_bus.max(br.to_bus));
                if br_pair == (from, to) {
                    br.r_ohm += r_add;
                    br.l_mh += l_add_h * 1000.0;
                    n_series_devices += 1;
                    tracing::debug!(
                        sd_id,
                        r_add,
                        l_add_mh = l_add_h * 1000.0,
                        from,
                        to,
                        "DCSeriesDevice resistance added to DcBranch"
                    );
                    break;
                }
            }
        }
    }

    // --- DCShunt → shunt conductance on DC bus ---
    //
    // CGMES: DCShunt is a DC filter bank (capacitor + ESR).  At DC steady-state
    // only the resistive component (ESR) matters.  G_shunt = 1/R adds to the
    // DC KCL equation.
    let mut n_dc_shunts = 0u32;
    for (dsh_id, dsh_obj) in objects.iter().filter(|(_, o)| o.class == "DCShunt") {
        let r_shunt = dsh_obj.parse_f64("resistance").unwrap_or(0.0);
        if r_shunt < 1e-9 {
            continue;
        }
        let g_shunt = 1.0 / r_shunt;
        // Find which DCNode this shunt is connected to.
        for node_id in &dc_node_ids {
            if let Some(eq_list) = idx.dcnode_to_eq.get(node_id.as_str())
                && eq_list.iter().any(|(eid, _)| eid == dsh_id)
                && let Some(&bus_id) = dcnode_bus_id.get(node_id.as_str())
            {
                // Find the DcBus and add shunt conductance.
                if let Some(dc_bus) = network.hvdc.find_dc_bus_mut(bus_id) {
                    dc_bus.g_shunt_siemens += g_shunt;
                    n_dc_shunts += 1;
                    tracing::debug!(
                        dsh_id,
                        r_shunt,
                        g_shunt,
                        bus_id,
                        "DCShunt conductance added to DcBus"
                    );
                }
                break;
            }
        }
    }

    // --- DCSwitch / DCBreaker → DC branch status enforcement ---
    //
    // CGMES: DCSwitch and DCBreaker control DC-side topology.  An open switch
    // disconnects the associated DCLineSegment (sets DcBranch.status = false).
    let mut n_dc_switches_open = 0u32;
    for (sw_id, sw_obj) in objects.iter().filter(|(_, o)| {
        matches!(
            o.class.as_str(),
            "DCSwitch" | "DCBreaker" | "DCDisconnector"
        )
    }) {
        // Check SSH open state.
        let is_open = sw_obj
            .get_ref("open")
            .map(|v| v == "true")
            .or_else(|| sw_obj.parse_f64("open").map(|v| v > 0.5))
            .unwrap_or(false);
        if !is_open {
            continue;
        }
        // Find the two DCNodes this switch connects via DCTerminal.
        let mut endpoints: Vec<u32> = Vec::new();
        for node_id in &dc_node_ids {
            if let Some(eq_list) = idx.dcnode_to_eq.get(node_id.as_str())
                && eq_list.iter().any(|(eid, _)| eid == sw_id)
                && let Some(&bus_id) = dcnode_bus_id.get(node_id.as_str())
            {
                endpoints.push(bus_id);
            }
        }
        if endpoints.len() >= 2 {
            let (from, to) = (
                endpoints[0].min(endpoints[1]),
                endpoints[0].max(endpoints[1]),
            );
            // Disable the matching DcBranch.
            for br in network.hvdc.dc_branches_mut() {
                let br_pair = (br.from_bus.min(br.to_bus), br.from_bus.max(br.to_bus));
                if br_pair == (from, to) {
                    br.status = false;
                    n_dc_switches_open += 1;
                    tracing::info!(
                        sw_id,
                        from,
                        to,
                        class = sw_obj.class.as_str(),
                        "Open DCSwitch/DCBreaker → DcBranch disabled"
                    );
                    break;
                }
            }
        }
    }

    // --- DCGround → ground return resistance on DC bus ---
    //
    // CGMES: DCGround represents the earth electrode and ground return path
    // in monopole HVDC or asymmetric bipole operation.  The grounding resistance
    // creates a path from the DC bus to earth (V=0), adding G_ground to KCL.
    let mut n_dc_grounds = 0u32;
    for (dg_id, dg_obj) in objects.iter().filter(|(_, o)| o.class == "DCGround") {
        let r_ground = dg_obj.parse_f64("r").unwrap_or(0.0);
        if r_ground < 1e-12 {
            continue;
        }
        // Find which DCNode this ground is connected to.
        for node_id in &dc_node_ids {
            if let Some(eq_list) = idx.dcnode_to_eq.get(node_id.as_str())
                && eq_list.iter().any(|(eid, _)| eid == dg_id)
                && let Some(&bus_id) = dcnode_bus_id.get(node_id.as_str())
            {
                if let Some(dc_bus) = network.hvdc.find_dc_bus_mut(bus_id) {
                    // Multiple DCGround objects on one bus → parallel: accumulate as
                    // conductance (G = 1/R).  Store back as equivalent R_parallel.
                    let g_existing = if dc_bus.r_ground_ohm > 0.0 {
                        1.0 / dc_bus.r_ground_ohm
                    } else {
                        0.0
                    };
                    let g_total = g_existing + 1.0 / r_ground;
                    dc_bus.r_ground_ohm = 1.0 / g_total;
                    n_dc_grounds += 1;
                    tracing::debug!(dg_id, r_ground, bus_id, "DCGround resistance set on DcBus");
                }
                break;
            }
        }
    }

    // --- DCBusbar → validate DC bus topology (informational) ---
    let n_dc_busbars: usize = objects.values().filter(|o| o.class == "DCBusbar").count();

    // --- DCTopologicalIsland → validate DC grid grouping ---
    let n_dc_islands: usize = objects
        .values()
        .filter(|o| o.class == "DCTopologicalIsland")
        .count();
    if n_dc_islands > 0 && n_dc_islands as u32 != grid_counter {
        tracing::warn!(
            cgmes_islands = n_dc_islands,
            surge_grids = grid_counter,
            "DCTopologicalIsland count differs from BFS-derived DC grid count"
        );
    }

    tracing::info!(
        dc_buses = network.hvdc.dc_bus_count(),
        dc_converters = handled_convs.len(),
        dc_branches = network.hvdc.dc_branch_count(),
        dc_series_devices = n_series_devices,
        dc_shunts = n_dc_shunts,
        dc_switches_open = n_dc_switches_open,
        dc_grounds = n_dc_grounds,
        dc_busbars = n_dc_busbars,
        dc_islands = n_dc_islands,
        "CGMES DC topology → Surge DC network"
    );

    handled_convs
}

pub(crate) fn group_dc_into_grids(_network: &mut Network) {
    // The canonical network model keeps explicit DC topology in `network.hvdc`.
    // Source-format specific MTDC grouping is now handled only inside I/O adapters.
}
