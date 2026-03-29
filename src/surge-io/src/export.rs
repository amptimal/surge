// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Network model CSV export for ML training pipelines (PLAN-083 / P5-049).
//!
//! Writes power system network models and solution snapshots to a structured
//! column-oriented CSV format — one file per table. This serves the same role
//! as HDF5 for ML training datasets: named datasets, row-major numeric arrays,
//! and an index manifest mapping dataset names to files.
//!
//! The output schema mirrors the HDF5 group layout recommended in the roadmap:
//!
//! ```text
//! <output_dir>/
//!   buses.csv       — bus_id, base_kv, bus_type, v_mag_pu, v_ang_rad,
//!                     p_load_pu, q_load_pu, p_gen_pu, q_gen_pu
//!   branches.csv    — from_bus, to_bus, r_pu, x_pu, b_pu, tap, shift_deg,
//!                     rate_a_mva, p_flow_mw, q_flow_mvar, loading_pct
//!   generators.csv  — bus, pg_mw, qg_mvar, p_max_mw, p_min_mw,
//!                     q_max_mvar, q_min_mvar, vg_pu, in_service
//!   manifest.json   — dataset names → file paths
//! ```
//!
//! `write_solution_snapshot` writes a single flat CSV with all bus voltages,
//! branch flows, and convergence metadata columns side-by-side.

use std::collections::HashMap;
use std::path::Path;

use surge_network::Network;
use surge_network::network::BusType;
use surge_solution::PfSolution;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("directory creation failed for '{path}': {source}")]
    DirCreate {
        path: String,
        source: std::io::Error,
    },
}

/// Write a network model to a directory of CSV files.
///
/// Creates `buses.csv`, `branches.csv`, `generators.csv`, and `manifest.json`
/// in `output_dir`. The directory is created if it does not exist.
///
/// # Schema
///
/// **buses.csv**: `bus_id, base_kv, bus_type_code, v_mag_pu, v_ang_rad,
///   p_load_pu, q_load_pu, p_gen_pu, q_gen_pu`
///
/// **branches.csv**: `from_bus, to_bus, r_pu, x_pu, b_pu, tap, shift_deg,
///   rate_a_mva, in_service, p_flow_mw, q_flow_mvar, loading_pct`
///
/// **generators.csv**: `bus, pg_mw, qg_mvar, p_max_mw, p_min_mw,
///   q_max_mvar, q_min_mvar, vg_pu, in_service`
///
/// `p_load_pu` and `q_load_pu` are in per-unit on `network.base_mva`.
/// `p_gen_pu` and `q_gen_pu` are the sum of all in-service generators at
/// each bus, also in per-unit.
///
/// Branch flow columns (`p_flow_mw`, `q_flow_mvar`, `loading_pct`) are NaN
/// when no OPF/PF solution is attached to the branch struct.
pub fn write_network_csv(network: &Network, output_dir: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(output_dir).map_err(|e| Error::DirCreate {
        path: output_dir.to_string_lossy().into_owned(),
        source: e,
    })?;

    let base = network.base_mva;

    // --- Precompute per-bus demand from Load objects ---
    let bus_demand_p = network.bus_load_p_mw();
    let bus_demand_q = network.bus_load_q_mvar();

    // --- Build bus-level generator aggregates ---
    let bus_map = network.bus_index_map();
    let mut bus_pg_pu = vec![0.0f64; network.buses.len()];
    let mut bus_qg_pu = vec![0.0f64; network.buses.len()];
    for g in &network.generators {
        if g.in_service
            && let Some(&idx) = bus_map.get(&g.bus)
        {
            bus_pg_pu[idx] += g.p / base;
            bus_qg_pu[idx] += g.q / base;
        }
    }

    // --- buses.csv ---
    let buses_path = output_dir.join("buses.csv");
    let mut wtr = csv::Writer::from_path(&buses_path)?;
    wtr.write_record([
        "bus_id",
        "base_kv",
        "bus_type_code",
        "v_mag_pu",
        "v_ang_rad",
        "p_load_pu",
        "q_load_pu",
        "p_gen_pu",
        "q_gen_pu",
    ])?;
    for (i, bus) in network.buses.iter().enumerate() {
        let bus_type_code = match bus.bus_type {
            BusType::PQ => 1u8,
            BusType::PV => 2,
            BusType::Slack => 3,
            BusType::Isolated => 4,
        };
        wtr.write_record(&[
            bus.number.to_string(),
            format!("{:.6}", bus.base_kv),
            bus_type_code.to_string(),
            format!("{:.8}", bus.voltage_magnitude_pu),
            format!("{:.8}", bus.voltage_angle_rad),
            format!("{:.8}", bus_demand_p.get(i).copied().unwrap_or(0.0) / base),
            format!("{:.8}", bus_demand_q.get(i).copied().unwrap_or(0.0) / base),
            format!("{:.8}", bus_pg_pu[i]),
            format!("{:.8}", bus_qg_pu[i]),
        ])?;
    }
    wtr.flush()?;

    // --- branches.csv ---
    let branches_path = output_dir.join("branches.csv");
    let mut wtr = csv::Writer::from_path(&branches_path)?;
    wtr.write_record([
        "from_bus",
        "to_bus",
        "r_pu",
        "x_pu",
        "b_pu",
        "tap",
        "shift_deg",
        "rate_a_mva",
        "in_service",
        "p_flow_mw",
        "q_flow_mvar",
        "loading_pct",
    ])?;
    for br in &network.branches {
        let p_flow = f64::NAN;
        let q_flow = f64::NAN;
        let loading = f64::NAN;

        let fmt_f64 = |v: f64| -> String {
            if v.is_nan() {
                "NaN".to_string()
            } else {
                format!("{v:.8}")
            }
        };

        wtr.write_record(&[
            br.from_bus.to_string(),
            br.to_bus.to_string(),
            format!("{:.8}", br.r),
            format!("{:.8}", br.x),
            format!("{:.8}", br.b),
            format!("{:.8}", br.tap),
            format!("{:.6}", br.phase_shift_rad.to_degrees()),
            format!("{:.3}", br.rating_a_mva),
            (br.in_service as u8).to_string(),
            fmt_f64(p_flow),
            fmt_f64(q_flow),
            fmt_f64(loading),
        ])?;
    }
    wtr.flush()?;

    // --- generators.csv ---
    let generators_path = output_dir.join("generators.csv");
    let mut wtr = csv::Writer::from_path(&generators_path)?;
    wtr.write_record([
        "bus",
        "pg_mw",
        "qg_mvar",
        "p_max_mw",
        "p_min_mw",
        "q_max_mvar",
        "q_min_mvar",
        "vg_pu",
        "in_service",
    ])?;
    for g in &network.generators {
        let qmax = if g.qmax.abs() > 1e6 { f64::NAN } else { g.qmax };
        let qmin = if g.qmin.abs() > 1e6 { f64::NAN } else { g.qmin };
        let pmax = if g.pmax.abs() > 1e6 { f64::NAN } else { g.pmax };
        let fmt_f64 = |v: f64| -> String {
            if v.is_nan() {
                "NaN".to_string()
            } else {
                format!("{v:.6}")
            }
        };
        wtr.write_record(&[
            g.bus.to_string(),
            format!("{:.6}", g.p),
            format!("{:.6}", g.q),
            fmt_f64(pmax),
            format!("{:.6}", g.pmin),
            fmt_f64(qmax),
            fmt_f64(qmin),
            format!("{:.8}", g.voltage_setpoint_pu),
            (g.in_service as u8).to_string(),
        ])?;
    }
    wtr.flush()?;

    // --- manifest.json ---
    let manifest: HashMap<&str, &str> = [
        ("buses", "buses.csv"),
        ("branches", "branches.csv"),
        ("generators", "generators.csv"),
    ]
    .into_iter()
    .collect();
    let manifest_path = output_dir.join("manifest.json");
    let manifest_json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(manifest_path, manifest_json)?;

    Ok(())
}

/// Write a power flow solution snapshot to a single CSV file.
///
/// Each row corresponds to one bus. Branch flow and convergence metadata are
/// stored as repeated values in columns (one row per bus; branch-count columns
/// would explode the schema — branch flows are written to a second file).
///
/// Output layout (two files):
///
/// - `<path>` — bus-level snapshot:
///   `bus_id, v_mag_pu, v_ang_rad, p_inject_pu, q_inject_pu, island_id`
///
/// - `<path_stem>_branches.<ext>` — branch-level snapshot:
///   `from_bus, to_bus, p_flow_mw, q_flow_mvar, s_mva, loading_pct`
///
/// Convergence metadata (status, iterations, max_mismatch, solve_time_secs)
/// is written to a `<path_stem>_meta.json` sidecar file.
///
/// # Arguments
/// - `result` — power flow solution
/// - `network` — the network the solution was computed on (needed for bus numbers
///   and branch ratings)
/// - `path` — base path; must end in `.csv`
pub fn write_solution_snapshot(
    result: &PfSolution,
    network: &Network,
    path: &Path,
) -> Result<(), Error> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| Error::DirCreate {
            path: parent.to_string_lossy().into_owned(),
            source: e,
        })?;
    }

    // --- Bus snapshot ---
    let mut wtr = csv::Writer::from_path(path)?;
    wtr.write_record([
        "bus_id",
        "v_mag_pu",
        "v_ang_rad",
        "p_inject_pu",
        "q_inject_pu",
        "island_id",
    ])?;

    let n_buses = result.voltage_magnitude_pu.len().min(network.buses.len());
    for i in 0..n_buses {
        let bus_id = if i < result.bus_numbers.len() {
            result.bus_numbers[i]
        } else {
            network.buses[i].number
        };
        let island_id = if i < result.island_ids.len() {
            result.island_ids[i].to_string()
        } else {
            "0".to_string()
        };
        let p_inj = if i < result.active_power_injection_pu.len() {
            result.active_power_injection_pu[i]
        } else {
            0.0
        };
        let q_inj = if i < result.reactive_power_injection_pu.len() {
            result.reactive_power_injection_pu[i]
        } else {
            0.0
        };
        wtr.write_record(&[
            bus_id.to_string(),
            format!("{:.8}", result.voltage_magnitude_pu[i]),
            format!("{:.8}", result.voltage_angle_rad[i]),
            format!("{:.8}", p_inj),
            format!("{:.8}", q_inj),
            island_id,
        ])?;
    }
    wtr.flush()?;

    // --- Branch snapshot ---
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let ext = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let branch_filename = format!("{stem}_branches.{ext}");
    let branch_path = parent.join(&branch_filename);

    let branch_flows = result.branch_pq_flows();
    let s_flows = result.branch_apparent_power();
    let loadings = result.branch_loading_pct(network).unwrap_or_default();

    let mut wtr = csv::Writer::from_path(&branch_path)?;
    wtr.write_record([
        "from_bus",
        "to_bus",
        "p_flow_mw",
        "q_flow_mvar",
        "s_mva",
        "loading_pct",
    ])?;
    for (i, br) in network.branches.iter().enumerate() {
        let (p_mw, q_mvar) = if i < branch_flows.len() {
            branch_flows[i]
        } else {
            (0.0, 0.0)
        };
        let s_mva = if i < s_flows.len() { s_flows[i] } else { 0.0 };
        let loading = if i < loadings.len() { loadings[i] } else { 0.0 };
        wtr.write_record(&[
            br.from_bus.to_string(),
            br.to_bus.to_string(),
            format!("{p_mw:.6}"),
            format!("{q_mvar:.6}"),
            format!("{s_mva:.6}"),
            format!("{loading:.4}"),
        ])?;
    }
    wtr.flush()?;

    // --- Metadata sidecar ---
    let meta_filename = format!("{stem}_meta.json");
    let meta_path = parent.join(meta_filename);
    let status_str = format!("{:?}", result.status);
    let meta = serde_json::json!({
        "status": status_str,
        "iterations": result.iterations,
        "max_mismatch_pu": result.max_mismatch,
        "solve_time_secs": result.solve_time_secs,
        "n_buses": n_buses,
        "n_branches": network.branches.len(),
        "n_islands": result.n_islands(),
        "q_limited_buses": result.q_limited_buses,
        "n_q_limit_switches": result.n_q_limit_switches,
    });
    std::fs::write(meta_path, serde_json::to_string_pretty(&meta)?)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};
    use surge_solution::{PfSolution, SolveStatus};

    /// Build the standard IEEE 14-bus test network with reasonable parameters.
    fn ieee14_network() -> Network {
        let mut net = Network::new("ieee14_synthetic");
        net.base_mva = 100.0;

        // Buses
        let bus_data: &[(u32, BusType, f64, f64, f64, f64)] = &[
            (1, BusType::Slack, 345.0, 0.0, 0.0, 1.060),
            (2, BusType::PV, 345.0, 21.7, 12.7, 1.045),
            (3, BusType::PQ, 345.0, 94.2, 19.0, 1.010),
            (4, BusType::PQ, 345.0, 47.8, -3.9, 1.019),
            (5, BusType::PQ, 345.0, 7.6, 1.6, 1.020),
            (6, BusType::PV, 138.0, 11.2, 7.5, 1.070),
            (7, BusType::PQ, 138.0, 0.0, 0.0, 1.062),
            (8, BusType::PV, 138.0, 0.0, 0.0, 1.090),
            (9, BusType::PQ, 138.0, 29.5, 16.6, 1.056),
            (10, BusType::PQ, 138.0, 9.0, 5.8, 1.051),
            (11, BusType::PQ, 138.0, 3.5, 1.8, 1.057),
            (12, BusType::PQ, 138.0, 6.1, 1.6, 1.055),
            (13, BusType::PQ, 138.0, 13.5, 5.8, 1.050),
            (14, BusType::PQ, 138.0, 14.9, 5.0, 1.036),
        ];
        for &(num, btype, kv, pd, qd, vm) in bus_data {
            let mut b = Bus::new(num, btype, kv);
            b.voltage_magnitude_pu = vm;
            net.buses.push(b);
            if pd.abs() > 1e-10 || qd.abs() > 1e-10 {
                net.loads.push(Load::new(num, pd, qd));
            }
        }

        // Generators
        let gen_data: &[(u32, f64, f64, f64)] = &[
            (1, 232.4, 1.060, 332.4),
            (2, 40.0, 1.045, 140.0),
            (3, 0.0, 1.010, 100.0),
            (6, 0.0, 1.070, 100.0),
            (8, 0.0, 1.090, 100.0),
        ];
        for &(bus, pg, vs, pmax) in gen_data {
            let mut g = Generator::new(bus, pg, vs);
            g.pmax = pmax;
            g.qmax = 100.0;
            g.qmin = -100.0;
            net.generators.push(g);
        }

        // Branches
        let branch_data: &[(u32, u32, f64, f64, f64)] = &[
            (1, 2, 0.01938, 0.05917, 0.0528),
            (1, 5, 0.05403, 0.22304, 0.0492),
            (2, 3, 0.04699, 0.19797, 0.0438),
            (2, 4, 0.05811, 0.17632, 0.0374),
            (2, 5, 0.05695, 0.17388, 0.0340),
            (3, 4, 0.06701, 0.17103, 0.0346),
            (4, 5, 0.01335, 0.04211, 0.0128),
            (4, 7, 0.0, 0.20912, 0.0),
            (4, 9, 0.0, 0.55618, 0.0),
            (5, 6, 0.0, 0.25202, 0.0),
            (6, 11, 0.09498, 0.19890, 0.0),
            (6, 12, 0.12291, 0.25581, 0.0),
            (6, 13, 0.06615, 0.13027, 0.0),
            (7, 8, 0.0, 0.17615, 0.0),
            (7, 9, 0.0, 0.11001, 0.0),
            (9, 10, 0.03181, 0.08450, 0.0),
            (9, 14, 0.12711, 0.27038, 0.0),
            (10, 11, 0.08205, 0.19207, 0.0),
            (11, 12, 0.22092, 0.19988, 0.0),
            (12, 13, 0.17093, 0.34802, 0.0),
            (13, 14, 0.09111, 0.18049, 0.0),
        ];
        for &(f, t, r, x, b) in branch_data {
            net.branches.push(Branch::new_line(f, t, r, x, b));
        }

        net
    }

    #[test]
    fn write_network_csv_roundtrip() {
        let net = ieee14_network();
        let dir = std::env::temp_dir().join("surge_network_csv_test");
        let _ = std::fs::remove_dir_all(&dir);

        write_network_csv(&net, &dir).expect("write_network_csv failed");

        // buses.csv should have header + 14 data rows
        let buses_path = dir.join("buses.csv");
        assert!(buses_path.exists(), "buses.csv not found");
        let content = std::fs::read_to_string(&buses_path).unwrap();
        let row_count = content.lines().count();
        assert_eq!(
            row_count,
            1 + 14,
            "buses.csv: expected 15 lines (header + 14 buses), got {row_count}"
        );

        // branches.csv should have header + 21 data rows
        let branches_path = dir.join("branches.csv");
        assert!(branches_path.exists(), "branches.csv not found");
        let content = std::fs::read_to_string(&branches_path).unwrap();
        let branch_row_count = content.lines().count();
        assert_eq!(
            branch_row_count,
            1 + 21,
            "branches.csv: expected 22 lines (header + 21 branches), got {branch_row_count}"
        );

        // generators.csv should have header + 5 data rows
        let gen_path = dir.join("generators.csv");
        assert!(gen_path.exists(), "generators.csv not found");
        let content = std::fs::read_to_string(&gen_path).unwrap();
        let gen_row_count = content.lines().count();
        assert_eq!(
            gen_row_count,
            1 + 5,
            "generators.csv: expected 6 lines (header + 5 generators), got {gen_row_count}"
        );

        // manifest.json must list all three datasets
        let manifest_path = dir.join("manifest.json");
        assert!(manifest_path.exists(), "manifest.json not found");
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert!(manifest.get("buses").is_some());
        assert!(manifest.get("branches").is_some());
        assert!(manifest.get("generators").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn write_solution_snapshot_produces_files() {
        let net = ieee14_network();

        // Build a minimal but plausible solution (not actually solved, just non-empty)
        let n = net.n_buses();
        let b = net.n_branches();
        let mut sol = PfSolution::default();
        sol.status = SolveStatus::Converged;
        sol.iterations = 5;
        sol.max_mismatch = 1.23e-10;
        sol.solve_time_secs = 0.0012;
        sol.voltage_magnitude_pu = vec![1.0; n];
        sol.voltage_angle_rad = vec![0.0; n];
        sol.active_power_injection_pu = vec![0.1; n];
        sol.reactive_power_injection_pu = vec![0.05; n];
        sol.bus_numbers = net.buses.iter().map(|b| b.number).collect();
        sol.island_ids = vec![0usize; n];
        let (pf, pt, qf, qt) = surge_solution::compute_branch_power_flows(
            &net,
            &sol.voltage_magnitude_pu,
            &sol.voltage_angle_rad,
            net.base_mva,
        );
        sol.branch_p_from_mw = pf;
        sol.branch_p_to_mw = pt;
        sol.branch_q_from_mvar = qf;
        sol.branch_q_to_mvar = qt;

        let snap_path = std::env::temp_dir().join("surge_snap_test.csv");
        let _ = std::fs::remove_file(&snap_path);

        write_solution_snapshot(&sol, &net, &snap_path).expect("write_solution_snapshot failed");

        // Bus snapshot
        assert!(snap_path.exists(), "bus snapshot CSV not created");
        let content = std::fs::read_to_string(&snap_path).unwrap();
        let rows = content.lines().count();
        assert_eq!(rows, 1 + n, "expected header + {n} bus rows, got {rows}");

        // Branch snapshot
        let branch_snap = std::env::temp_dir().join("surge_snap_test_branches.csv");
        assert!(branch_snap.exists(), "branch snapshot CSV not created");
        let bcontent = std::fs::read_to_string(&branch_snap).unwrap();
        let brows = bcontent.lines().count();
        assert_eq!(
            brows,
            1 + b,
            "expected header + {b} branch rows, got {brows}"
        );

        // Metadata sidecar
        let meta_path = std::env::temp_dir().join("surge_snap_test_meta.json");
        assert!(meta_path.exists(), "metadata JSON not created");
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
        assert_eq!(meta["status"], "Converged");
        assert_eq!(meta["iterations"], 5);

        let _ = std::fs::remove_file(&snap_path);
        let _ = std::fs::remove_file(&branch_snap);
        let _ = std::fs::remove_file(&meta_path);
    }
}
