// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Simultaneous multi-interface transfer capability via LP.

use surge_network::Network;
use surge_sparse::Triplet;

use crate::dfax::PreparedTransferModel;
use crate::error::TransferError;
use crate::types::{MultiTransferRequest, MultiTransferResult};

/// Compute simultaneous transfer capability across multiple interfaces via LP.
///
/// Prepares the required PTDF rows and base-case flows internally from
/// `network`, then solves a single LP maximizing `Σ weight[i] × T[i]` subject
/// to all monitored branch thermal limits. This is the canonical entry point
/// for multi-interface transfer studies.
pub fn compute_multi_transfer(
    network: &Network,
    request: &MultiTransferRequest,
) -> Result<MultiTransferResult, TransferError> {
    PreparedTransferModel::new(network)
        .map_err(TransferError::from)?
        .compute_multi_transfer(request)
}

pub(crate) fn solve_multi_interface_transfer_lp(
    base_mva: f64,
    active_branches: &[usize],
    base_flows: &[f64],
    ratings: &[f64],
    net_ptdf_matrix: &[Vec<f64>],
    weights: &[f64],
    max_transfer_mw: &[f64],
) -> Result<MultiTransferResult, TransferError> {
    let n_iface = net_ptdf_matrix.len();
    if n_iface == 0 {
        return Err(TransferError::InvalidRequest(
            "at least one transfer path is required".to_string(),
        ));
    }

    let base = base_mva;

    let w: Vec<f64> = if weights.len() == n_iface {
        weights.to_vec()
    } else {
        vec![1.0; n_iface]
    };
    let max_t: Vec<f64> = if max_transfer_mw.len() == n_iface {
        max_transfer_mw.iter().map(|&m| m / base).collect()
    } else {
        vec![1e6 / base; n_iface]
    };

    let n_row = active_branches.len();
    let col_cost: Vec<f64> = w.iter().map(|weight| -weight).collect();
    let col_lower = vec![0.0; n_iface];
    let col_upper = max_t;

    let mut row_lower = Vec::with_capacity(n_row);
    let mut row_upper = Vec::with_capacity(n_row);
    for &branch_idx in active_branches {
        row_lower.push(-ratings[branch_idx] - base_flows[branch_idx]);
        row_upper.push(ratings[branch_idx] - base_flows[branch_idx]);
    }

    let mut triplets: Vec<Triplet<f64>> = Vec::new();
    for (iface_idx, net_ptdf) in net_ptdf_matrix.iter().enumerate() {
        for (row_idx, &value) in net_ptdf.iter().enumerate() {
            if value.abs() > 1e-15 {
                triplets.push(Triplet {
                    row: row_idx,
                    col: iface_idx,
                    val: value,
                });
            }
        }
    }

    let (a_start, a_index, a_value) =
        surge_opf::advanced::triplets_to_csc(&triplets, n_row, n_iface);

    let prob = surge_opf::backends::SparseProblem {
        n_col: n_iface,
        n_row,
        col_cost,
        col_lower,
        col_upper,
        row_lower,
        row_upper,
        a_start,
        a_index,
        a_value,
        q_start: None,
        q_index: None,
        q_value: None,
        integrality: None,
    };

    let solver = surge_opf::backends::try_default_lp_solver()
        .map_err(|e| TransferError::Solver(e.to_string()))?;
    let lp_opts = surge_opf::backends::LpOptions::default();
    let sol = solver
        .solve(&prob, &lp_opts)
        .map_err(|e| TransferError::Solver(e.to_string()))?;

    if !matches!(
        sol.status,
        surge_opf::backends::LpSolveStatus::Optimal
            | surge_opf::backends::LpSolveStatus::SubOptimal
    ) {
        return Err(TransferError::Solver(format!(
            "LP solver did not converge: {:?}",
            sol.status
        )));
    }

    let transfer_mw: Vec<f64> = sol.x.iter().map(|&transfer| transfer * base).collect();
    let total_weighted_transfer = transfer_mw
        .iter()
        .zip(w.iter())
        .map(|(transfer, weight)| transfer * weight)
        .sum();

    let net_branch_transfer: Vec<f64> = active_branches
        .iter()
        .enumerate()
        .map(|(row_idx, _)| {
            net_ptdf_matrix
                .iter()
                .enumerate()
                .map(|(iface_idx, net_ptdf)| net_ptdf[row_idx] * sol.x[iface_idx])
                .sum()
        })
        .collect();

    let binding_branch: Vec<Option<usize>> = (0..n_iface)
        .map(|iface_idx| {
            let mut best: Option<(usize, f64)> = None;
            for (row_idx, &branch_idx) in active_branches.iter().enumerate() {
                let sensitivity = net_ptdf_matrix[iface_idx][row_idx];
                let slack_upper =
                    (ratings[branch_idx] - base_flows[branch_idx]) - net_branch_transfer[row_idx];
                let slack_lower =
                    net_branch_transfer[row_idx] - (-ratings[branch_idx] - base_flows[branch_idx]);
                let min_slack = slack_upper.min(slack_lower);
                if min_slack < 1e-6 {
                    let abs_contribution = (sensitivity * sol.x[iface_idx]).abs();
                    if abs_contribution > 1e-9
                        && (best.is_none() || abs_contribution > best.expect("best exists").1)
                    {
                        best = Some((branch_idx, abs_contribution));
                    }
                }
            }
            best.map(|(branch_idx, _)| branch_idx)
        })
        .collect();

    Ok(MultiTransferResult {
        transfer_mw,
        binding_branch,
        total_weighted_transfer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::case_path;
    use crate::types::TransferPath;

    fn load_case9() -> Network {
        surge_io::load(case_path("case9")).expect("failed to parse case9")
    }

    fn transfer_path(name: &str, source_buses: Vec<u32>, sink_buses: Vec<u32>) -> TransferPath {
        TransferPath::new(name, source_buses, sink_buses)
    }

    #[test]
    fn test_multi_interface_transfer_matches_prepared_model() {
        let net = load_case9();
        let interfaces = vec![
            transfer_path("bus2_to_bus7", vec![2], vec![7]),
            transfer_path("bus3_to_bus9", vec![3], vec![9]),
        ];
        let weights = vec![1.0, 2.0];
        let max_transfer_mw = vec![500.0, 600.0];
        let request = MultiTransferRequest {
            paths: interfaces.clone(),
            weights: Some(weights.clone()),
            max_transfer_mw: Some(max_transfer_mw.clone()),
        };

        let wrapped =
            compute_multi_transfer(&net, &request).expect("wrapped multi-interface transfer");
        let prepared = PreparedTransferModel::new(&net)
            .expect("prepared transfer model")
            .compute_multi_transfer(&request)
            .expect("prepared multi-interface transfer");

        assert_eq!(wrapped.transfer_mw.len(), interfaces.len());
        assert_eq!(wrapped.binding_branch.len(), interfaces.len());
        assert_eq!(wrapped.binding_branch, prepared.binding_branch);
        assert!((wrapped.total_weighted_transfer - prepared.total_weighted_transfer).abs() < 1e-10);
        for (wrapped_transfer, prepared_transfer) in
            wrapped.transfer_mw.iter().zip(prepared.transfer_mw.iter())
        {
            assert!((wrapped_transfer - prepared_transfer).abs() < 1e-10);
        }
    }

    #[test]
    fn test_multi_transfer_binding_branch_uses_total_branch_flow() {
        let result = solve_multi_interface_transfer_lp(
            1.0,
            &[0],
            &[0.0],
            &[1.0],
            &[vec![1.0], vec![1.0]],
            &[1.0, 1.0],
            &[0.5, 0.5],
        )
        .expect("multi-transfer LP should solve");

        assert_eq!(result.transfer_mw, vec![0.5, 0.5]);
        assert_eq!(result.binding_branch, vec![Some(0), Some(0)]);
        assert!((result.total_weighted_transfer - 1.0).abs() < 1e-12);
    }

    #[test]
    fn test_multi_transfer_ignores_out_of_service_rated_branches() {
        let mut low_rating = load_case9();
        low_rating.branches[8].in_service = false;
        low_rating.branches[8].rating_a_mva = 0.001;

        let mut high_rating = low_rating.clone();
        high_rating.branches[8].rating_a_mva = 1_000_000.0;

        let request = MultiTransferRequest {
            paths: vec![transfer_path("bus2_to_bus9", vec![2], vec![9])],
            weights: None,
            max_transfer_mw: Some(vec![500.0]),
        };

        let low_result = compute_multi_transfer(&low_rating, &request)
            .expect("multi-transfer should ignore out-of-service low-rated branch");
        let high_result = compute_multi_transfer(&high_rating, &request)
            .expect("multi-transfer should ignore out-of-service high-rated branch");

        assert!(
            (low_result.transfer_mw[0] - high_result.transfer_mw[0]).abs() < 1e-9,
            "out-of-service branch rating should not affect transfer: low={}, high={}",
            low_result.transfer_mw[0],
            high_result.transfer_mw[0]
        );
        assert_ne!(low_result.binding_branch[0], Some(8));
        assert_ne!(high_result.binding_branch[0], Some(8));
    }
}
