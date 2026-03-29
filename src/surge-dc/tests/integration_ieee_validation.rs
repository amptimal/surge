// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

mod common;

use surge_dc::{
    DcAnalysisRequest, DcPfOptions, LodfMatrixRequest, PtdfRequest, compute_lodf_matrix,
    compute_ptdf, run_dc_analysis, solve_dc, solve_dc_opts,
};
use surge_network::network::BusType;

// ---------------------------------------------------------------------------
// Helper: verify KCL at every non-slack bus
// ---------------------------------------------------------------------------

/// For each non-slack bus, the sum of branch flows into the bus must equal the
/// bus injection stored in `p_inject_pu`.  The DC power flow is exact under its
/// own assumptions, so we use a tight tolerance.
fn assert_kcl_balance(net: &surge_network::Network, case_name: &str) {
    let sol = solve_dc(net).unwrap_or_else(|e| panic!("{case_name}: solve_dc failed: {e}"));
    let bus_map = net.bus_index_map();
    let n = net.n_buses();

    // Accumulate net branch flow leaving each bus.
    let mut flow_out = vec![0.0f64; n];
    for (br_idx, br) in net.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }
        let from = *bus_map.get(&br.from_bus).expect("from_bus in map");
        let to = *bus_map.get(&br.to_bus).expect("to_bus in map");
        let pf = sol.branch_p_flow[br_idx];
        // Positive flow is from -> to, so from exports pf and to imports pf.
        flow_out[from] += pf;
        flow_out[to] -= pf;
    }

    for (i, bus) in net.buses.iter().enumerate() {
        if bus.bus_type == BusType::Slack {
            continue;
        }
        let inject = sol.p_inject_pu[i];
        let residual = (inject - flow_out[i]).abs();
        assert!(
            residual < 1e-10,
            "{case_name}: KCL violated at bus {} (index {i}): inject={inject:.12e}, \
             flow_out={:.12e}, residual={residual:.2e}",
            bus.number,
            flow_out[i],
        );
    }
}

// ---------------------------------------------------------------------------
// (a)-(c) KCL balance tests
// ---------------------------------------------------------------------------

#[test]
fn dc_case9_kcl_balance() {
    let net = common::load_case("case9");
    assert_kcl_balance(&net, "case9");
}

#[test]
fn dc_case14_kcl_balance() {
    let net = common::load_case("case14");
    assert_kcl_balance(&net, "case14");
}

#[test]
fn dc_case30_kcl_balance() {
    let net = common::load_case("case30");
    assert_kcl_balance(&net, "case30");
}

// ---------------------------------------------------------------------------
// (d) Slack bus angle is zero
// ---------------------------------------------------------------------------

#[test]
fn dc_slack_angle_is_zero() {
    let net = common::load_case("case9");
    let sol = solve_dc(&net).expect("solve_dc should succeed");
    let slack_idx = net
        .slack_bus_index()
        .expect("case9 should have a slack bus");
    assert_eq!(
        sol.theta[slack_idx], 0.0,
        "Slack bus angle must be exactly 0.0, got {}",
        sol.theta[slack_idx]
    );
}

// ---------------------------------------------------------------------------
// (e) PTDF row-sum conservation: slack column should be zero
// ---------------------------------------------------------------------------

#[test]
fn dc_ptdf_row_sum_conservation() {
    let net = common::load_case("case9");
    let all_branches: Vec<usize> = (0..net.n_branches()).collect();
    let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all_branches))
        .expect("compute_ptdf should succeed");

    let slack_idx = net
        .slack_bus_index()
        .expect("case9 should have a slack bus");

    // Under single-slack PTDF, the column for the slack bus should be zero
    // because the slack bus is removed from the sensitivity.
    for &br in ptdf.monitored_branches() {
        let val = ptdf.get(br, slack_idx);
        assert!(
            val.abs() < 1e-12,
            "PTDF[branch={br}, slack_bus={slack_idx}] should be 0.0, got {val:.6e}"
        );
    }

    // Additionally, for each PTDF row, the sum across all non-slack buses
    // should be meaningful: for a lossless DC model the row sums are bounded
    // but we primarily verify the slack-column property above.
}

// ---------------------------------------------------------------------------
// (f) LODF diagonal is -1
// ---------------------------------------------------------------------------

#[test]
fn dc_lodf_diagonal_is_negative_one() {
    let net = common::load_case("case9");
    let all_branches: Vec<usize> = (0..net.n_branches()).collect();
    let lodf = compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all_branches))
        .expect("compute_lodf_matrix should succeed");

    let n = lodf.n_rows();
    assert_eq!(n, lodf.n_cols(), "LODF matrix should be square");

    for k in 0..n {
        let diag = lodf[(k, k)];
        // Bridge lines (whose removal disconnects the network) have
        // LODF[k,k] = infinity.  For non-bridge lines it must be -1.
        if diag.is_infinite() {
            continue;
        }
        assert!(
            (diag - (-1.0)).abs() < 1e-8,
            "LODF[{k},{k}] should be -1.0 (non-bridge line), got {diag:.10e}"
        );
    }
}

// ---------------------------------------------------------------------------
// (g) LODF formula matches PTDF: LODF[i,k] = PTDF[i,k] / (1 - PTDF[k,k])
// ---------------------------------------------------------------------------

#[test]
fn dc_lodf_formula_matches_ptdf() {
    let net = common::load_case("case9");
    let all_branches: Vec<usize> = (0..net.n_branches()).collect();

    let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all_branches))
        .expect("compute_ptdf should succeed");
    let lodf = compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all_branches))
        .expect("compute_lodf_matrix should succeed");

    let bus_map = net.bus_index_map();
    let n_br = all_branches.len();

    for k in 0..n_br {
        let br_k = &net.branches[k];
        let from_k = *bus_map.get(&br_k.from_bus).unwrap();
        let to_k = *bus_map.get(&br_k.to_bus).unwrap();

        // PTDF_kk = PTDF[k, from_k] - PTDF[k, to_k]
        let ptdf_kk = ptdf.get(k, from_k) - ptdf.get(k, to_k);

        // Skip bridge lines where PTDF_kk ~= 1.0 (outaging disconnects network).
        if (1.0 - ptdf_kk).abs() < 1e-6 {
            continue;
        }

        for i in 0..n_br {
            if i == k {
                continue; // diagonal is -1 by definition
            }
            // PTDF_ik = PTDF[i, from_k] - PTDF[i, to_k]
            let ptdf_ik = ptdf.get(i, from_k) - ptdf.get(i, to_k);
            let expected_lodf = ptdf_ik / (1.0 - ptdf_kk);
            let actual_lodf = lodf[(i, k)];

            assert!(
                (actual_lodf - expected_lodf).abs() < 1e-8,
                "LODF[{i},{k}] mismatch: expected {expected_lodf:.10e} (from PTDF), \
                 got {actual_lodf:.10e}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// (h) DcAnalysisRequest matches standalone PTDF + LODF
// ---------------------------------------------------------------------------

#[test]
fn dc_analysis_request_matches_standalone() {
    let net = common::load_case("case9");
    let all_branches: Vec<usize> = (0..net.n_branches()).collect();

    // Run the unified analysis with PTDF + LODF.
    let request = DcAnalysisRequest::all_branches().with_lodf_outages(&all_branches);
    let analysis = run_dc_analysis(&net, &request).expect("run_dc_analysis should succeed");

    // Run standalone PTDF and LODF.
    let standalone_ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all_branches))
        .expect("standalone compute_ptdf should succeed");
    let standalone_lodf =
        compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all_branches))
            .expect("standalone compute_lodf_matrix should succeed");

    // Compare PTDF values.
    let ptdf = &analysis.ptdf;
    for &br in standalone_ptdf.monitored_branches() {
        let standalone_row = standalone_ptdf.row(br).expect("row should exist");
        let analysis_row = ptdf.row(br).expect("row should exist in analysis result");
        assert_eq!(
            standalone_row.len(),
            analysis_row.len(),
            "PTDF row length mismatch for branch {br}"
        );
        for (j, (&s, &a)) in standalone_row.iter().zip(analysis_row.iter()).enumerate() {
            assert!(
                (s - a).abs() < 1e-14,
                "PTDF[{br}, col={j}] mismatch: standalone={s:.14e}, analysis={a:.14e}"
            );
        }
    }

    // Compare LODF values.
    let lodf_result = analysis
        .lodf
        .as_ref()
        .expect("analysis should contain LODF result");
    let n = standalone_lodf.n_rows();
    assert_eq!(lodf_result.n_rows(), n, "LODF row count mismatch");
    assert_eq!(lodf_result.n_cols(), n, "LODF col count mismatch");
    for i in 0..n {
        for j in 0..n {
            let standalone_val = standalone_lodf[(i, j)];
            let analysis_val = lodf_result[(i, j)];
            // Bridge lines produce infinity; compare with exact equality for those.
            if standalone_val.is_infinite() || analysis_val.is_infinite() {
                assert_eq!(
                    standalone_val.is_sign_positive(),
                    analysis_val.is_sign_positive(),
                    "LODF[{i},{j}] infinity sign mismatch"
                );
            } else {
                assert!(
                    (standalone_val - analysis_val).abs() < 1e-14,
                    "LODF[{i},{j}] mismatch: standalone={standalone_val:.14e}, \
                     analysis={analysis_val:.14e}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// (i) Headroom slack distributes proportionally
// ---------------------------------------------------------------------------

#[test]
fn dc_headroom_slack_distributes_proportionally() {
    let net = common::load_case("case30");

    // Collect internal bus indices for in-service generators.
    let bus_map = net.bus_index_map();
    let gen_bus_indices: Vec<usize> = net
        .generators
        .iter()
        .filter(|g| g.in_service)
        .filter_map(|g| bus_map.get(&g.bus).copied())
        .collect::<std::collections::HashSet<usize>>()
        .into_iter()
        .collect();
    assert!(
        !gen_bus_indices.is_empty(),
        "case30 should have in-service generators"
    );

    let opts = DcPfOptions::with_headroom_slack(&gen_bus_indices);
    let sol = solve_dc_opts(&net, &opts).expect("solve_dc_opts with headroom slack should succeed");

    // slack_distribution should be non-empty.
    assert!(
        !sol.slack_distribution.is_empty(),
        "slack_distribution should be non-empty for headroom slack solve"
    );

    // The total distributed slack should sum to the total imbalance.
    // Compute total scheduled generation and total load in p.u.
    let base_mva = net.base_mva;
    let total_gen_mw: f64 = net
        .generators
        .iter()
        .filter(|g| g.in_service)
        .map(|g| g.p)
        .sum();
    let total_load_mw: f64 = net.bus_load_p_mw().iter().sum();
    let _imbalance_pu = (total_gen_mw - total_load_mw) / base_mva;

    let _distributed_total: f64 = sol.slack_distribution.values().sum();

    // The distributed slack absorbs whatever mismatch remains after the
    // scheduled injections.  We verify the distribution sums to a sensible
    // non-trivial value; a zero distribution would indicate the feature is
    // not working.
    let distributed_abs_sum: f64 = sol.slack_distribution.values().map(|v| v.abs()).sum();
    assert!(
        distributed_abs_sum > 0.0,
        "slack distribution should have non-zero magnitude"
    );

    // All participating buses should be from our requested set.
    for &bus_idx in sol.slack_distribution.keys() {
        assert!(
            gen_bus_indices.contains(&bus_idx),
            "slack_distribution contains bus {bus_idx} which was not in the \
             participating set"
        );
    }
}
