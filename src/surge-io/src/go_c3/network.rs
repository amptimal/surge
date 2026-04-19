// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO C3 → `surge_network::Network` structural conversion.
//!
//! Performs the straightforward structural mapping from the GO C3 data model
//! into Surge's canonical network types. Policy-driven decisions (commitment
//! mode, consumer modeling strategy, reserve zone wiring, voltage regulation)
//! are layered on by sibling modules (`enrich`, `reserves`, `voltage`,
//! `consumers`, `hvdc_q`) so that this conversion is lossless and
//! deterministic.
//!
//! Switchable-branch handling: this pass respects
//! [`GoC3Policy::is_branch_switchable`] when deciding whether a branch gets
//! `connection_cost`/`disconnection_cost` transition costs. Transition costs
//! are what the Rust SCUC engine uses as a per-branch switchable flag.

use std::collections::HashMap;

use surge_network::Network;
use surge_network::network::{
    Branch, Bus, BusType, Generator,
    branch::{BranchOpfControl, PhaseMode, TapMode},
    discrete_control::SwitchedShuntOpf,
    hvdc::{HvdcLink, LccConverterTerminal, LccHvdcControlMode, LccHvdcLink},
};

use super::Error;
use super::context::{
    AcLineInitialState, BranchRef, DcLineInitialState, DcLineReactiveBounds, GoC3Context,
    GoC3DeviceKind, TransformerInitialState,
};
use super::policy::GoC3Policy;
use super::types::*;

/// Convert a [`GoC3Problem`] into a [`Network`] and associated context.
///
/// Bus UIDs are mapped to sequential bus numbers starting at 1. Producers
/// become generators, consumers become loads. The mapping context preserves
/// all UID associations for downstream dispatch and solution export.
///
/// This call uses [`GoC3Policy::default`]; use [`to_network_with_policy`] to
/// override policy-dependent decisions (notably which branches are
/// switchable).
pub fn to_network(problem: &GoC3Problem) -> Result<(Network, GoC3Context), Error> {
    to_network_with_policy(problem, &GoC3Policy::default())
}

/// Like [`to_network`] but consults the supplied policy for decisions that
/// affect network construction.
pub fn to_network_with_policy(
    problem: &GoC3Problem,
    policy: &GoC3Policy,
) -> Result<(Network, GoC3Context), Error> {
    let base_mva = problem.network.general.base_norm_mva;
    let mut net = Network::new("go_c3");
    net.base_mva = base_mva;

    let mut ctx = GoC3Context::new();

    // Python's `build_surge_network` assigns circuit IDs as a single
    // monotonically-increasing integer across AC lines and transformers
    // (via `next_circuit`). Downstream code (AC OPF, reconcile, branch
    // lookup) keys on these integer circuits, so matching Python
    // requires the same numbering. `circuit_counter` runs across
    // `convert_ac_lines` and `convert_transformers` in order.
    let mut circuit_counter: u32 = 1;

    // ── Buses ────────────────────────────────────────────────────────────
    convert_buses(&problem.network, &mut net, &mut ctx)?;

    // ── Devices (producers → generators, consumers → loads) ──────────────
    convert_devices(&problem.network, base_mva, &mut net, &mut ctx)?;

    // ── AC lines ─────────────────────────────────────────────────────────
    convert_ac_lines(
        &problem.network,
        base_mva,
        policy,
        &mut net,
        &mut ctx,
        &mut circuit_counter,
    )?;

    // ── Transformers ─────────────────────────────────────────────────────
    convert_transformers(
        &problem.network,
        base_mva,
        policy,
        &mut net,
        &mut ctx,
        &mut circuit_counter,
    )?;

    // ── DC lines → VSC HVDC links ────────────────────────────────────────
    convert_dc_lines(&problem.network, base_mva, &mut net, &mut ctx)?;

    // ── Shunts ───────────────────────────────────────────────────────────
    convert_shunts(&problem.network, base_mva, &mut net, &mut ctx)?;

    Ok((net, ctx))
}

// ─── Bus conversion ──────────────────────────────────────────────────────────

fn convert_buses(
    go_net: &GoC3Network,
    net: &mut Network,
    ctx: &mut GoC3Context,
) -> Result<(), Error> {
    for (i, go_bus) in go_net.bus.iter().enumerate() {
        let bus_number = (i + 1) as u32;
        ctx.bus_uid_to_number.insert(go_bus.uid.clone(), bus_number);
        ctx.bus_number_to_uid.insert(bus_number, go_bus.uid.clone());

        let bus_type = match go_bus.bus_type.as_deref() {
            Some("Slack") => BusType::Slack,
            Some("PV") => BusType::PV,
            Some("PQ") => BusType::PQ,
            Some("Notused") => BusType::Isolated,
            _ => BusType::PQ,
        };

        let mut bus = Bus::new(bus_number, bus_type, go_bus.base_nom_volt);
        bus.name = go_bus.uid.clone();
        bus.voltage_magnitude_pu = go_bus.initial_status.vm;
        bus.voltage_angle_rad = go_bus.initial_status.va;
        bus.voltage_min_pu = go_bus.vm_lb;
        bus.voltage_max_pu = go_bus.vm_ub;
        net.buses.push(bus);
    }

    // If no bus was marked Slack, promote the first PV bus (or bus 1) as a
    // minimal fallback so the base Network passes structural validation.
    // The enrichment pass (`enrich::apply_reactive_capability_slack`) may
    // later replace this with a smarter choice driven by the policy.
    if !net.buses.iter().any(|b| b.bus_type == BusType::Slack) {
        let idx = net
            .buses
            .iter()
            .position(|b| b.bus_type == BusType::PV)
            .or(Some(0));
        if let Some(i) = idx {
            if let Some(b) = net.buses.get_mut(i) {
                b.bus_type = BusType::Slack;
            }
        }
    }

    // Record whichever buses ended up as Slack after the fallback. The
    // enrichment pass will update this if it repromotes the slack.
    ctx.slack_bus_numbers = net
        .buses
        .iter()
        .filter(|b| b.bus_type == BusType::Slack)
        .map(|b| b.number)
        .collect();

    Ok(())
}

// ─── Device conversion ───────────────────────────────────────────────────────

fn convert_devices(
    go_net: &GoC3Network,
    base_mva: f64,
    net: &mut Network,
    ctx: &mut GoC3Context,
) -> Result<(), Error> {
    // Count producers per bus for ordinal id generation. Consumers do NOT
    // become `surge_network::Load` objects: Python's `build_surge_network`
    // leaves the network load list empty and threads consumer demand
    // through `build_dispatch_request::_aggregate_bus_profiles` instead.
    // The Rust request builder follows the same convention, so adding
    // Load entries here would double-count demand.
    let mut gen_count_per_bus: HashMap<u32, u32> = HashMap::new();

    for dev in &go_net.simple_dispatchable_device {
        let bus_number = *ctx.bus_uid_to_number.get(&dev.bus).ok_or_else(|| {
            Error::Conversion(format!(
                "device '{}' references unknown bus '{}'",
                dev.uid, dev.bus
            ))
        })?;

        match dev.device_type {
            GoC3DeviceType::Producer => {
                let ordinal = gen_count_per_bus.entry(bus_number).or_insert(0);
                *ordinal += 1;
                let gen_id = dev.uid.clone();

                let p_mw = dev.initial_status.p * base_mva;
                let q_mvar = dev.initial_status.q * base_mva;

                let mut generator =
                    Generator::new(bus_number, p_mw, dev.vm_setpoint.unwrap_or(1.0));
                generator.id = gen_id.clone();
                // Match Python's `network.add_generator(..., machine_id="1")`
                // call. Downstream code keys on `(bus, machine_id)` so
                // leaving this as `None` silently breaks lookups.
                generator.machine_id = Some("1".to_string());
                generator.q = q_mvar;
                generator.pmin = 0.0; // Will be overridden by time series dispatch bounds.
                generator.pmax = 0.0; // Ditto.
                generator.qmin = -9999.0;
                generator.qmax = 9999.0;
                generator.machine_base_mva = base_mva;
                // GO C3 commitment is per-period, expressed via the
                // dispatch request's commitment options. The static
                // network `in_service` flag represents topology, not
                // commitment, so every producer is structurally in
                // service — mirrors Python's unconditional
                // `set_generator_in_service(uid, True)`.
                generator.in_service = true;

                net.generators.push(generator);
                ctx.device_uid_to_id
                    .insert(dev.uid.clone(), (bus_number, gen_id));
                ctx.device_kind_by_uid
                    .insert(dev.uid.clone(), GoC3DeviceKind::Producer);
            }
            GoC3DeviceType::Consumer => {
                // Consumer demand is carried via `DispatchProfiles::load`
                // in the dispatch request, not as static `Load` objects
                // on the network. Record the device-kind mapping so the
                // request builder can find the consumer, but don't
                // populate `network.loads`.
                let _ = base_mva;
                ctx.device_uid_to_id
                    .insert(dev.uid.clone(), (bus_number, dev.uid.clone()));
                ctx.device_kind_by_uid
                    .insert(dev.uid.clone(), GoC3DeviceKind::Consumer);
            }
        }
    }

    Ok(())
}

// ─── AC line conversion ──────────────────────────────────────────────────────

fn convert_ac_lines(
    go_net: &GoC3Network,
    base_mva: f64,
    policy: &GoC3Policy,
    net: &mut Network,
    ctx: &mut GoC3Context,
    circuit_counter: &mut u32,
) -> Result<(), Error> {
    for line in &go_net.ac_line {
        let from = resolve_bus(&line.fr_bus, &line.uid, ctx)?;
        let to = resolve_bus(&line.to_bus, &line.uid, ctx)?;
        let circuit_int = *circuit_counter;
        let circuit = circuit_int.to_string();
        *circuit_counter += 1;

        let rating_a = line.mva_ub_nom * base_mva;
        let rating_b = line.mva_ub_sht.unwrap_or(line.mva_ub_nom) * base_mva;
        let rating_c = line.mva_ub_em * base_mva;

        let mut br = Branch::new_line(from, to, line.r, line.x, line.b);
        br.circuit = circuit.clone();
        br.g_pi = line.g;
        br.rating_a_mva = rating_a;
        br.rating_b_mva = rating_b;
        br.rating_c_mva = rating_c;
        br.in_service = line.initial_status.on_status != 0;

        // GO C3 §4.4.6 eqs (62)-(63): connection/disconnection costs are
        // what the Rust SCUC engine uses as the per-branch switchable flag.
        // Only apply them when the policy says this branch is switchable.
        if policy.is_branch_switchable(&line.uid) {
            br.cost_startup = line.connection_cost;
            br.cost_shutdown = line.disconnection_cost;
        }

        // GO C3 §4.8 eqs (148)-(151): per-side shunt-to-ground components
        // when `additional_shunt == 1`.
        if line.additional_shunt != 0 {
            br.g_shunt_from = line.g_fr;
            br.b_shunt_from = line.b_fr;
            br.g_shunt_to = line.g_to;
            br.b_shunt_to = line.b_to;
        }

        let local_index = net.branches.len();
        net.branches.push(br);
        ctx.branch_uid_to_ref.insert(
            line.uid.clone(),
            BranchRef {
                from_bus: from,
                to_bus: to,
                circuit: circuit.clone(),
            },
        );
        ctx.branch_circuit_by_uid
            .insert(line.uid.clone(), circuit_int);
        ctx.branch_local_index_by_uid
            .insert(line.uid.clone(), local_index);
        ctx.ac_line_initial.insert(
            line.uid.clone(),
            AcLineInitialState {
                on_status: line.initial_status.on_status,
            },
        );
    }

    Ok(())
}

// ─── Transformer conversion ──────────────────────────────────────────────────

fn convert_transformers(
    go_net: &GoC3Network,
    base_mva: f64,
    policy: &GoC3Policy,
    net: &mut Network,
    ctx: &mut GoC3Context,
    circuit_counter: &mut u32,
) -> Result<(), Error> {
    for xfmr in &go_net.two_winding_transformer {
        let from = resolve_bus(&xfmr.fr_bus, &xfmr.uid, ctx)?;
        let to = resolve_bus(&xfmr.to_bus, &xfmr.uid, ctx)?;
        let circuit_int = *circuit_counter;
        let circuit = circuit_int.to_string();
        *circuit_counter += 1;

        let init = &xfmr.initial_status;

        let rating_a = xfmr.mva_ub_nom * base_mva;
        let rating_b = xfmr.mva_ub_sht.unwrap_or(xfmr.mva_ub_nom) * base_mva;
        let rating_c = xfmr.mva_ub_em * base_mva;

        // Python's `network.add_branch(...)` call in adapter.py is used
        // for both AC lines and transformers without tagging the branch
        // type as Transformer. The Surge AC OPF uses `tap` and
        // `phase_shift_rad` directly as off-nominal parameters; the
        // `branch_type` field is metadata only. We leave it as `Line`
        // to keep the AC reconcile handoff byte-identical to Python.
        let mut br = Branch::new_line(from, to, xfmr.r, xfmr.x, xfmr.b);
        br.tap = init.tm;
        br.phase_shift_rad = init.ta;
        br.circuit = circuit.clone();
        br.g_mag = xfmr.g;
        br.rating_a_mva = rating_a;
        br.rating_b_mva = rating_b;
        br.rating_c_mva = rating_c;
        br.in_service = init.on_status != 0;

        if policy.is_branch_switchable(&xfmr.uid) {
            br.cost_startup = xfmr.connection_cost;
            br.cost_shutdown = xfmr.disconnection_cost;
        }

        if xfmr.additional_shunt != 0 {
            br.g_shunt_from = xfmr.g_fr;
            br.b_shunt_from = xfmr.b_fr;
            br.g_shunt_to = xfmr.g_to;
            br.b_shunt_to = xfmr.b_to;
        }

        // ── Tap and phase-shift bounds → OPF control ──
        let tm_lb = xfmr.tm_lb.unwrap_or(init.tm);
        let tm_ub = xfmr.tm_ub.unwrap_or(init.tm);
        let ta_lb = xfmr.ta_lb.unwrap_or(init.ta);
        let ta_ub = xfmr.ta_ub.unwrap_or(init.ta);

        let tap_movable = (tm_ub - tm_lb).abs() > 1e-9;
        let phase_movable = (ta_ub - ta_lb).abs() > 1e-9;

        if tap_movable || phase_movable {
            let mut opf_ctrl = BranchOpfControl::default();
            if tap_movable {
                opf_ctrl.tap_mode = TapMode::Continuous;
                opf_ctrl.tap_min = tm_lb;
                opf_ctrl.tap_max = tm_ub;
            }
            if phase_movable {
                opf_ctrl.phase_mode = PhaseMode::Continuous;
                opf_ctrl.phase_min_rad = ta_lb;
                opf_ctrl.phase_max_rad = ta_ub;
            }
            br.opf_control = Some(opf_ctrl);
        }

        let local_index = net.branches.len();
        net.branches.push(br);
        ctx.branch_uid_to_ref.insert(
            xfmr.uid.clone(),
            BranchRef {
                from_bus: from,
                to_bus: to,
                circuit: circuit.clone(),
            },
        );
        ctx.branch_circuit_by_uid
            .insert(xfmr.uid.clone(), circuit_int);
        ctx.branch_local_index_by_uid
            .insert(xfmr.uid.clone(), local_index);
        ctx.transformer_initial.insert(
            xfmr.uid.clone(),
            TransformerInitialState {
                on_status: init.on_status,
                tm: init.tm,
                ta: init.ta,
            },
        );
        ctx.transformer_tap_bounds
            .insert(xfmr.uid.clone(), (tm_lb, tm_ub));
    }

    Ok(())
}

// ─── DC line → VSC HVDC ─────────────────────────────────────────────────────

fn convert_dc_lines(
    go_net: &GoC3Network,
    base_mva: f64,
    net: &mut Network,
    ctx: &mut GoC3Context,
) -> Result<(), Error> {
    for dc in &go_net.dc_line {
        let from_bus = resolve_bus(&dc.fr_bus, &dc.uid, ctx)?;
        let to_bus = resolve_bus(&dc.to_bus, &dc.uid, ctx)?;

        let init = &dc.initial_status;

        // Store the per-unit Q bounds (matches Python `dc_line_q_bounds`).
        let qdc_fr_lb_pu = dc.qdc_fr_lb.unwrap_or(init.qdc_fr);
        let qdc_fr_ub_pu = dc.qdc_fr_ub.unwrap_or(init.qdc_fr);
        let qdc_to_lb_pu = dc.qdc_to_lb.unwrap_or(init.qdc_to);
        let qdc_to_ub_pu = dc.qdc_to_ub.unwrap_or(init.qdc_to);

        // GO C3 DC lines are modeled as LCC HVDC with p_dc_min/max bounds.
        // Mirrors Python `adapter.py::build_surge_network` lines 3137-3157
        // which calls `network.add_lcc_dc_line(...)`. The P bounds come
        // from `pdc_ub` (upper) and `pdc_lb` (or `-pdc_ub` as default).
        // Reactive terminal resources (at `fr` and `to` buses) are added
        // separately in `hvdc_q.rs::apply_hvdc_reactive_terminals` when
        // the policy formulation is AC.
        let pdc_ub_mw = dc.pdc_ub * base_mva;
        let pdc_lb_mw = -dc.pdc_ub.abs() * base_mva;

        let mut link = LccHvdcLink {
            name: dc.uid.clone(),
            mode: LccHvdcControlMode::PowerControl,
            scheduled_setpoint: init.pdc_fr * base_mva,
            scheduled_voltage_kv: 500.0,
            resistance_ohm: 0.0,
            p_dc_min_mw: pdc_lb_mw,
            p_dc_max_mw: pdc_ub_mw,
            ..LccHvdcLink::default()
        };
        link.rectifier = LccConverterTerminal {
            bus: from_bus,
            ..LccConverterTerminal::default()
        };
        link.inverter = LccConverterTerminal {
            bus: to_bus,
            ..LccConverterTerminal::default()
        };

        net.hvdc.links.push(HvdcLink::Lcc(link));
        ctx.dc_line_uid_to_name
            .insert(dc.uid.clone(), dc.uid.clone());
        ctx.dc_line_initial.insert(
            dc.uid.clone(),
            DcLineInitialState {
                pdc_fr: init.pdc_fr,
                qdc_fr: init.qdc_fr,
                qdc_to: init.qdc_to,
            },
        );
        ctx.dc_line_q_bounds.insert(
            dc.uid.clone(),
            DcLineReactiveBounds {
                qdc_fr_lb: qdc_fr_lb_pu,
                qdc_fr_ub: qdc_fr_ub_pu,
                qdc_to_lb: qdc_to_lb_pu,
                qdc_to_ub: qdc_to_ub_pu,
            },
        );

        // Populate the reactive-support resource-ID mappings here
        // (unconditionally, even in DC mode) so the DC-mode context
        // that `solve_baseline_scenario` passes to `export_go3_solution`
        // can route synthetic DC terminal Q awards back to the
        // `qdc_fr` / `qdc_to` solution slots. Python's adapter populates
        // these mappings in its main DC-line loop BEFORE the AC-mode
        // gate that adds the actual synth generators — the mappings are
        // needed for solution export regardless of whether the network
        // was built in AC or DC mode. The synth generators themselves
        // are only added by `hvdc_q::apply_hvdc_reactive_terminals`
        // when the policy formulation is AC.
        let fr_resource_id = format!("__dc_line_q__{}__fr", dc.uid);
        let to_resource_id = format!("__dc_line_q__{}__to", dc.uid);
        ctx.dc_line_reactive_support_resource_ids.insert(
            dc.uid.clone(),
            super::context::DcLineReactiveSupportResources {
                fr: fr_resource_id.clone(),
                to: to_resource_id.clone(),
            },
        );
        ctx.dc_line_reactive_support_resource_to_output
            .insert(fr_resource_id.clone(), (dc.uid.clone(), "qdc_fr".into()));
        ctx.dc_line_reactive_support_resource_to_output
            .insert(to_resource_id.clone(), (dc.uid.clone(), "qdc_to".into()));
        // Python also classifies the synth resources as "producer_static"
        // in the device-kind map even when they aren't added to the
        // network as generators (DC mode). Mirror that so downstream
        // helpers that iterate `device_kind_by_uid` see the same set.
        ctx.device_kind_by_uid
            .insert(fr_resource_id, GoC3DeviceKind::ProducerStatic);
        ctx.device_kind_by_uid
            .insert(to_resource_id, GoC3DeviceKind::ProducerStatic);
    }

    Ok(())
}

// ─── Shunt conversion ────────────────────────────────────────────────────────

fn convert_shunts(
    go_net: &GoC3Network,
    base_mva: f64,
    net: &mut Network,
    ctx: &mut GoC3Context,
) -> Result<(), Error> {
    let mut ordinal_by_bus: HashMap<u32, u32> = HashMap::new();
    for shunt in &go_net.shunt {
        let bus_number = *ctx.bus_uid_to_number.get(&shunt.bus).ok_or_else(|| {
            Error::Conversion(format!(
                "shunt '{}' references unknown bus '{}'",
                shunt.uid, shunt.bus
            ))
        })?;

        let step_lb = shunt.step_lb.unwrap_or(shunt.initial_status.step as f64) as i32;
        let step_ub = shunt.step_ub.unwrap_or(shunt.initial_status.step as f64) as i32;
        let is_switchable = step_lb != step_ub;

        // Capture initial step and bounds in the context (used by the
        // dispatch request builder and solution rounding).
        ctx.shunt_initial_steps
            .insert(shunt.uid.clone(), shunt.initial_status.step);
        ctx.shunt_step_bounds
            .insert(shunt.uid.clone(), (step_lb, step_ub));

        if is_switchable {
            // Model as OPF-relaxed switched shunt with continuous bounds.
            let b_lo = shunt.bs * step_lb as f64;
            let b_hi = shunt.bs * step_ub as f64;
            let b_init = shunt.bs * shunt.initial_status.step as f64;
            let b_step = if shunt.bs.abs() > 1e-15 {
                shunt.bs.abs()
            } else {
                0.0
            };

            let ss = SwitchedShuntOpf {
                id: shunt.uid.clone(),
                bus: bus_number,
                b_min_pu: b_lo.min(b_hi),
                b_max_pu: b_lo.max(b_hi),
                b_init_pu: b_init,
                b_step_pu: b_step,
            };
            net.controls.switched_shunts_opf.push(ss);

            // GO C3 shunt `gs` is not optimized. When non-zero, pin it as
            // a static bus conductance term at the initial step (mirrors
            // Python adapter.py:2821-2824).
            if shunt.gs.abs() > 1e-12 {
                let step = shunt.initial_status.step as f64;
                if let Some(bus) = net.buses.iter_mut().find(|b| b.number == bus_number) {
                    bus.shunt_conductance_mw += shunt.gs * step * base_mva;
                }
            }

            // Record the control-ID → shunt UID mapping used by the solution
            // exporter to map OPF susceptance solutions back to GO C3 steps.
            let ordinal = ordinal_by_bus.entry(bus_number).or_insert(0);
            *ordinal += 1;
            let control_id = format!("switched_shunt_opf_{}_{}", bus_number, ordinal);
            ctx.switched_shunt_control_id_to_uid
                .insert(control_id, shunt.uid.clone());
        } else {
            // Fixed shunt — add as bus shunt admittance.
            let step = shunt.initial_status.step as f64;
            let bus = net
                .buses
                .iter_mut()
                .find(|b| b.number == bus_number)
                .ok_or_else(|| {
                    Error::Conversion(format!(
                        "shunt '{}' bus {} not found in network",
                        shunt.uid, bus_number
                    ))
                })?;
            bus.shunt_conductance_mw += shunt.gs * step * base_mva;
            bus.shunt_susceptance_mvar += shunt.bs * step * base_mva;
        }

        ctx.shunt_uid_to_id
            .insert(shunt.uid.clone(), (bus_number, shunt.uid.clone()));
    }

    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn resolve_bus(uid: &str, element_uid: &str, ctx: &GoC3Context) -> Result<u32, Error> {
    ctx.bus_uid_to_number.get(uid).copied().ok_or_else(|| {
        Error::Conversion(format!(
            "element '{}' references unknown bus '{}'",
            element_uid, uid
        ))
    })
}
