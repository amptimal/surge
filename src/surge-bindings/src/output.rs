// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Output formatting helpers for CLI text and JSON output.

use anyhow::Result;

use crate::cli::{ResolvedTextDetail, TextDetail};

pub(crate) fn save_network(
    network: &surge_network::Network,
    path: &std::path::Path,
    format_override: Option<&str>,
) -> Result<()> {
    match format_override.map(str::to_ascii_lowercase).as_deref() {
        None => surge_io::save(network, path)?,
        Some("matpower") | Some("m") => surge_io::matpower::save(network, path)?,
        Some("psse") | Some("psse33") | Some("raw") => {
            surge_io::psse::raw::save(network, path, 33)?
        }
        Some("psse35") => surge_io::psse::raw::save(network, path, 35)?,
        Some("json") | Some("surge-json") => surge_io::json::save(network, path)?,
        Some("bin") | Some("surge-bin") => surge_io::bin::save(network, path)?,
        Some("xiidm") | Some("iidm") => surge_io::xiidm::save(network, path)?,
        Some("dss") => surge_io::dss::save(network, path)?,
        Some("epc") => surge_io::epc::save(network, path)?,
        Some("uct") | Some("ucte") => surge_io::ucte::save(network, path)?,
        Some("cgmes") => surge_io::cgmes::save(network, path, surge_io::cgmes::Version::V2_4_15)?,
        Some("cgmes3") => surge_io::cgmes::save(network, path, surge_io::cgmes::Version::V3_0)?,
        Some(other) => anyhow::bail!(
            "unsupported export format override '{other}'; use matpower, psse33, psse35, \
             surge-json, surge-bin, xiidm, dss, epc, ucte, cgmes, or cgmes3"
        ),
    }
    Ok(())
}

/// Print a serializable value as pretty JSON to stdout.
///
/// serde_json serialization fails when the value contains f64::NAN or f64::INFINITY,
/// which are not valid JSON numbers. Diverged power flow solutions can contain NaN/Inf
/// in voltage or mismatch arrays. This wrapper catches the error, prints an actionable
/// message to stderr, and exits with code 2 ("solve failed / diverged") so callers
/// can distinguish solver failure (2) from input error (1).
pub(crate) fn print_json_result(v: &impl serde::Serialize) {
    match serde_json::to_string_pretty(v) {
        Ok(json) => println!("{}", json),
        Err(e) => {
            tracing::error!(
                "JSON serialization failed (possible NaN/Inf in results): {}. \
                 Tip: the solve may have diverged — check the solver status field.",
                e
            );
            println!(
                "{{\"status\":\"diverged\",\"error\":\"JSON serialization failed: NaN or Inf in results\"}}"
            );
            std::process::exit(2);
        }
    }
}

pub(crate) fn resolve_text_detail(
    detail: TextDetail,
    network: &surge_network::Network,
) -> ResolvedTextDetail {
    match detail {
        TextDetail::Summary => ResolvedTextDetail::Summary,
        TextDetail::Full => ResolvedTextDetail::Full,
        TextDetail::Auto => {
            if network.n_buses() <= 30 && network.n_branches() <= 50 {
                ResolvedTextDetail::Full
            } else {
                ResolvedTextDetail::Summary
            }
        }
    }
}

pub(crate) fn print_voltage_range_summary(network: &surge_network::Network, vm: &[f64]) {
    if vm.is_empty() || network.buses.is_empty() {
        return;
    }
    let (min_idx, min_vm) = vm
        .iter()
        .copied()
        .enumerate()
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .unwrap();
    let (max_idx, max_vm) = vm
        .iter()
        .copied()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .unwrap();
    println!(
        "Voltage range: {:.4} p.u. (bus {}) to {:.4} p.u. (bus {})",
        min_vm, network.buses[min_idx].number, max_vm, network.buses[max_idx].number
    );
}

pub(crate) fn print_angle_range_summary(network: &surge_network::Network, va_rad: &[f64]) {
    if va_rad.is_empty() || network.buses.is_empty() {
        return;
    }
    let (min_idx, min_angle) = va_rad
        .iter()
        .copied()
        .enumerate()
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .unwrap();
    let (max_idx, max_angle) = va_rad
        .iter()
        .copied()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .unwrap();
    println!(
        "Angle range: {:.4} deg (bus {}) to {:.4} deg (bus {})",
        min_angle.to_degrees(),
        network.buses[min_idx].number,
        max_angle.to_degrees(),
        network.buses[max_idx].number
    );
}

pub(crate) fn print_branch_loading_summary(
    network: &surge_network::Network,
    loading_pct: &[f64],
    limit: usize,
) {
    let mut entries: Vec<(usize, f64)> = loading_pct
        .iter()
        .copied()
        .enumerate()
        .filter(|(idx, val)| {
            val.is_finite()
                && network
                    .branches
                    .get(*idx)
                    .map(|branch| branch.in_service)
                    .unwrap_or(false)
        })
        .collect();

    entries.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    if entries.is_empty() {
        println!("Rated branch loading: unavailable (no positive Rate A limits in service)");
        return;
    }

    let (peak_idx, peak_loading) = entries[0];
    let peak_branch = &network.branches[peak_idx];
    println!(
        "Peak rated branch loading: {:.2}% on {} -> {}",
        peak_loading, peak_branch.from_bus, peak_branch.to_bus
    );

    println!("Top branch loading:");
    for (idx, loading) in entries.into_iter().take(limit) {
        let branch = &network.branches[idx];
        println!(
            "  {} -> {} [{}]  {:>7.2}%",
            branch.from_bus, branch.to_bus, branch.circuit, loading
        );
    }
}

pub(crate) fn print_generator_dispatch_summary(
    network: &surge_network::Network,
    gen_p_mw: &[f64],
    limit: usize,
) {
    let mut entries: Vec<(u32, String, f64)> = network
        .generators
        .iter()
        .filter(|generator| generator.in_service)
        .zip(gen_p_mw.iter().copied())
        .map(|(generator, pg)| {
            (
                generator.bus,
                generator
                    .machine_id
                    .clone()
                    .unwrap_or_else(|| "1".to_string()),
                pg,
            )
        })
        .collect();

    if entries.is_empty() {
        return;
    }

    entries.sort_by(|a, b| b.2.total_cmp(&a.2));

    println!("Top generator dispatch:");
    for (bus, machine_id, pg) in entries.into_iter().take(limit) {
        println!("  Bus {:>6} [{}]  {:>10.1} MW", bus, machine_id, pg);
    }
}

pub(crate) fn print_lmp_summary(network: &surge_network::Network, lmp: &[f64], limit: usize) {
    let mut entries: Vec<(u32, f64)> = network
        .buses
        .iter()
        .zip(lmp.iter().copied())
        .map(|(bus, value)| (bus.number, value))
        .collect();

    if entries.is_empty() {
        return;
    }

    entries.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    println!("Top LMP buses:");
    for (bus, value) in entries.into_iter().take(limit) {
        println!("  Bus {:>6}  {:>10.4} $/MWh", bus, value);
    }
}

pub(crate) fn print_power_balance_summary(
    total_generation_mw: f64,
    total_load_mw: f64,
    total_losses_mw: f64,
) {
    println!(
        "Generation / load / losses: {:.1} / {:.1} / {:.1} MW",
        total_generation_mw, total_load_mw, total_losses_mw
    );
}

pub(crate) fn print_solver_identity(name: Option<&str>, version: Option<&str>) {
    match (name, version) {
        (Some(name), Some(version)) => println!("Solver: {name} {version}"),
        (Some(name), None) => println!("Solver: {name}"),
        _ => {}
    }
}

/// Bus voltage table for Full-mode text output.
///
/// Used by ACPF, AcpfWarm (5 columns with P), and FDPF (4 columns without P).
pub(crate) fn print_bus_voltage_table(
    network: &surge_network::Network,
    vm: &[f64],
    va_rad: &[f64],
    p_injection_pu: Option<&[f64]>,
) {
    println!("\nBus Voltages:");
    if p_injection_pu.is_some() {
        println!(
            "{:>6}  {:>8}  {:>10}  {:>10}  {:>10}",
            "Bus", "Type", "Vm(p.u.)", "Va(deg)", "P(p.u.)"
        );
    } else {
        println!(
            "{:>6}  {:>8}  {:>10}  {:>10}",
            "Bus", "Type", "Vm(p.u.)", "Va(deg)"
        );
    }
    for (i, bus) in network.buses.iter().enumerate() {
        if let Some(p_inj) = p_injection_pu {
            println!(
                "{:>6}  {:>8}  {:>10.6}  {:>10.4}  {:>10.4}",
                bus.number,
                format!("{:?}", bus.bus_type),
                vm[i],
                va_rad[i].to_degrees(),
                p_inj[i]
            );
        } else {
            println!(
                "{:>6}  {:>8}  {:>10.6}  {:>10.4}",
                bus.number,
                format!("{:?}", bus.bus_type),
                vm[i],
                va_rad[i].to_degrees(),
            );
        }
    }
}

/// Generator dispatch table for Full-mode text output.
///
/// Used by DC-OPF, SCOPF (4 columns), and AC-OPF (5 columns with Qg).
pub(crate) fn print_generator_dispatch_table(
    network: &surge_network::Network,
    gen_p_mw: &[f64],
    gen_q_mvar: Option<&[f64]>,
) {
    println!("\nGenerator Dispatch:");
    if gen_q_mvar.is_some() {
        println!(
            "{:>6}  {:>10}  {:>10}  {:>10}  {:>10}",
            "Bus", "Pg(MW)", "Qg(MVAr)", "Pmin", "Pmax"
        );
    } else {
        println!(
            "{:>6}  {:>10}  {:>10}  {:>10}",
            "Bus", "Pg(MW)", "Pmin", "Pmax"
        );
    }
    let gen_indices: Vec<usize> = network
        .generators
        .iter()
        .enumerate()
        .filter(|(_, g)| g.in_service)
        .map(|(i, _)| i)
        .collect();
    for (j, &gi) in gen_indices.iter().enumerate() {
        let g = &network.generators[gi];
        if let Some(q) = gen_q_mvar {
            println!(
                "{:>6}  {:>10.1}  {:>10.1}  {:>10.1}  {:>10.1}",
                g.bus, gen_p_mw[j], q[j], g.pmin, g.pmax
            );
        } else {
            println!(
                "{:>6}  {:>10.1}  {:>10.1}  {:>10.1}",
                g.bus, gen_p_mw[j], g.pmin, g.pmax
            );
        }
    }
}

pub(crate) fn max_severity(r: &surge_contingency::ContingencyResult) -> f64 {
    r.violations
        .iter()
        .map(|v| match v {
            surge_contingency::Violation::ThermalOverload { loading_pct, .. } => *loading_pct,
            surge_contingency::Violation::NonConvergent { .. } => f64::MAX,
            surge_contingency::Violation::VoltageLow { vm, limit, .. } => {
                (limit - vm) / limit * 100.0
            }
            surge_contingency::Violation::VoltageHigh { vm, limit, .. } => {
                (vm - limit) / limit * 100.0
            }
            surge_contingency::Violation::Islanding { .. } => 50.0,
            surge_contingency::Violation::FlowgateOverload { loading_pct, .. } => *loading_pct,
            surge_contingency::Violation::InterfaceOverload { loading_pct, .. } => *loading_pct,
        })
        .fold(0.0_f64, f64::max)
}

pub(crate) fn worst_violation_summary(r: &surge_contingency::ContingencyResult) -> String {
    for v in &r.violations {
        if let surge_contingency::Violation::NonConvergent {
            max_mismatch,
            iterations,
        } = v
        {
            if max_mismatch.is_finite() {
                return format!("NON-CONVERGENT (mismatch={max_mismatch:.2e}, iters={iterations})");
            }
            return "NON-CONVERGENT (solver failure)".to_string();
        }
    }
    for v in &r.violations {
        if let surge_contingency::Violation::ThermalOverload {
            from_bus,
            to_bus,
            loading_pct,
            ..
        } = v
        {
            return format!("Thermal: {}->{} at {:.1}%", from_bus, to_bus, loading_pct);
        }
    }
    for v in &r.violations {
        match v {
            surge_contingency::Violation::VoltageLow { bus_number, vm, .. } => {
                return format!("V_low: bus {} at {:.4} p.u.", bus_number, vm);
            }
            surge_contingency::Violation::VoltageHigh { bus_number, vm, .. } => {
                return format!("V_high: bus {} at {:.4} p.u.", bus_number, vm);
            }
            _ => {}
        }
    }
    String::new()
}

pub(crate) fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let end = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= max_len.saturating_sub(3))
        .last()
        .unwrap_or(0);
    format!("{}...", &s[..end])
}
