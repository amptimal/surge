// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Synthetic reactive-support generators for DC line terminals (AC mode).
//!
//! In AC formulations, GO C3 DC lines carry independent reactive-power
//! setpoints at each terminal (`qdc_fr`, `qdc_to`) that must participate
//! in the AC power flow just like a local generator's reactive injection.
//! Surge's dispatch engine does not express per-converter reactive
//! dispatch as a first-class decision variable, so the Python adapter
//! injects a pair of synthetic "reactive-support producer" generators —
//! one per DC-line terminal — that carry the reactive capability envelope
//! and zero active-power capability. The adapter then routes the
//! dispatch's `q_mvar` award on each synthetic generator back to the DC
//! line output in the solution exporter.
//!
//! This pass runs after `network.rs` has added the base VSC-HVDC links
//! and before `voltage.rs` sweeps the network for voltage-control
//! fallbacks — the synthetic generators need to be in `Network.generators`
//! when `voltage.rs` walks the adjacency graph, and they must be marked
//! excluded from voltage regulation so the fallback doesn't try to make
//! them the Slack anchor for a component.
//!
//! Mirrors Python `markets/go_c3/adapter.py::build_surge_network` lines
//! 3108-3211.

use surge_network::Network;
use surge_network::market::CostCurve;
use surge_network::network::{Generator, MarketParams};

use super::Error;
use super::context::GoC3Context;
use super::policy::{GoC3Formulation, GoC3Policy};
use super::types::*;

/// Qualification flag set on synthetic DC-line reactive generators so the
/// voltage-control fallback (in `voltage.rs`) doesn't pick them as the
/// component slack anchor. Mirrors Python's
/// `_set_generator_excluded_from_voltage_regulation`.
const AC_VOLTAGE_REGULATION_EXCLUDED: &str = "ac_voltage_regulation_excluded";

/// Add synthetic reactive-support generators at each DC line terminal.
///
/// This is a no-op when the policy formulation is not AC: the DC dispatch
/// does not see reactive variables, so injecting these would waste solver
/// time.
pub fn apply_hvdc_reactive_terminals(
    network: &mut Network,
    context: &mut GoC3Context,
    problem: &GoC3Problem,
    policy: &GoC3Policy,
) -> Result<(), Error> {
    if policy.formulation != GoC3Formulation::Ac {
        return Ok(());
    }

    let base_mva = problem.network.general.base_norm_mva;
    let periods = problem.time_series_input.general.time_periods;

    for dc in &problem.network.dc_line {
        let Some(&fr_bus_number) = context.bus_uid_to_number.get(&dc.fr_bus) else {
            continue;
        };
        let Some(&to_bus_number) = context.bus_uid_to_number.get(&dc.to_bus) else {
            continue;
        };
        let Some(q_bounds) = context.dc_line_q_bounds.get(&dc.uid) else {
            continue;
        };
        let q_bounds = q_bounds.clone();

        // Resource IDs and their context mappings are populated by
        // `network::convert_dc_lines` so that the DC-mode context used
        // by `export_go3_solution` already has them. Here we just add
        // the generators themselves (AC mode only).
        let fr_resource_id = dc_line_reactive_support_resource_id(&dc.uid, "fr");
        let to_resource_id = dc_line_reactive_support_resource_id(&dc.uid, "to");

        // ── From terminal ───────────────────────────────────────────
        let (fr_qmin_mvar, fr_qmax_mvar) =
            terminal_generator_q_bounds_mvar(q_bounds.qdc_fr_lb, q_bounds.qdc_fr_ub, base_mva);
        let fr_q_capability = fr_qmin_mvar.abs().max(fr_qmax_mvar.abs());
        if fr_q_capability > 1e-9 {
            let fr_bus_vm = bus_voltage_magnitude(network, fr_bus_number);
            let mut generator = Generator::new(fr_bus_number, 0.0, fr_bus_vm);
            generator.id = fr_resource_id.clone();
            generator.pmin = 0.0;
            generator.pmax = 0.0;
            generator.qmin = fr_qmin_mvar;
            generator.qmax = fr_qmax_mvar;
            generator.machine_base_mva = base_mva;
            generator.in_service = true;
            generator.voltage_regulated = false;
            generator.reg_bus = None;
            generator.machine_id = Some("1".to_string());
            attach_zero_cost_curve(&mut generator);
            mark_excluded_from_voltage_regulation(&mut generator);
            network.generators.push(generator);

            context
                .internal_support_commitment_schedule
                .insert(fr_resource_id, vec![true; periods]);
        }

        // ── To terminal ─────────────────────────────────────────────
        let (to_qmin_mvar, to_qmax_mvar) =
            terminal_generator_q_bounds_mvar(q_bounds.qdc_to_lb, q_bounds.qdc_to_ub, base_mva);
        let to_q_capability = to_qmin_mvar.abs().max(to_qmax_mvar.abs());
        if to_q_capability > 1e-9 {
            let to_bus_vm = bus_voltage_magnitude(network, to_bus_number);
            let mut generator = Generator::new(to_bus_number, 0.0, to_bus_vm);
            generator.id = to_resource_id.clone();
            generator.pmin = 0.0;
            generator.pmax = 0.0;
            generator.qmin = to_qmin_mvar;
            generator.qmax = to_qmax_mvar;
            generator.machine_base_mva = base_mva;
            generator.in_service = true;
            generator.voltage_regulated = false;
            generator.reg_bus = None;
            generator.machine_id = Some("1".to_string());
            attach_zero_cost_curve(&mut generator);
            mark_excluded_from_voltage_regulation(&mut generator);
            network.generators.push(generator);

            context
                .internal_support_commitment_schedule
                .insert(to_resource_id, vec![true; periods]);
        }
    }

    Ok(())
}

/// Format the synthetic resource ID. Mirrors Python
/// `_dc_line_reactive_support_resource_id`.
fn dc_line_reactive_support_resource_id(dc_line_uid: &str, terminal: &str) -> String {
    let terminal_key = if terminal == "fr" { "fr" } else { "to" };
    format!("__dc_line_q__{}__{}", dc_line_uid, terminal_key)
}

/// Mirrors `_dc_line_terminal_generator_q_bounds_mvar`. GO C3 stores DC
/// line reactive bounds from the converter's perspective (positive = out
/// of the converter). Surge generators inject Q into the bus, so the sign
/// flips.
fn terminal_generator_q_bounds_mvar(q_lb_pu: f64, q_ub_pu: f64, base_mva: f64) -> (f64, f64) {
    let mut qmin_mvar = -q_ub_pu * base_mva;
    let mut qmax_mvar = -q_lb_pu * base_mva;
    if qmin_mvar > qmax_mvar {
        std::mem::swap(&mut qmin_mvar, &mut qmax_mvar);
    }
    (qmin_mvar, qmax_mvar)
}

fn bus_voltage_magnitude(network: &Network, bus_number: u32) -> f64 {
    network
        .buses
        .iter()
        .find(|b| b.number == bus_number)
        .map(|b| {
            if b.voltage_magnitude_pu > 1e-9 {
                b.voltage_magnitude_pu
            } else {
                1.0
            }
        })
        .unwrap_or(1.0)
}

fn mark_excluded_from_voltage_regulation(generator: &mut Generator) {
    let market = generator.market.get_or_insert_with(MarketParams::default);
    market
        .qualifications
        .insert(AC_VOLTAGE_REGULATION_EXCLUDED.to_string(), true);
}

/// Attach a zero-cost polynomial curve so the AC OPF doesn't reject the
/// synthetic generator for having no cost model. Python's dispatch-request
/// builder emits an equivalent zero-segment offer schedule; setting the
/// static cost is the Rust-idiomatic equivalent.
fn attach_zero_cost_curve(generator: &mut Generator) {
    generator.cost = Some(CostCurve::Polynomial {
        startup: 0.0,
        shutdown: 0.0,
        coeffs: vec![0.0, 0.0],
    });
}
