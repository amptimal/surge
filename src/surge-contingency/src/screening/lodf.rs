// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! LODF screening built on the canonical `surge-dc` streaming interfaces.

use surge_network::Network;
use tracing::warn;

use crate::{ThermalRating, get_rating};

/// Screen all contingencies using streamed LODF outage columns.
///
/// This keeps the contingency crate off the dense all-pairs path and reuses
/// the canonical `surge-dc` LODF engine.
pub fn screen_with_sparse_lodf(
    network: &Network,
    contingencies: &[surge_network::network::Contingency],
    base_dc_flows: &[f64],
    screening_pct: f64,
    thermal_rating: ThermalRating,
) -> (Vec<usize>, usize) {
    let n_br = network.n_branches();
    let base_mva = network.base_mva;
    let all_branches: Vec<usize> = (0..n_br).collect();

    let mut model = match surge_dc::PreparedDcStudy::new(network) {
        Ok(model) => model,
        Err(e) => {
            warn!("DC model preparation failed, skipping LODF screening: {e}");
            let all_critical: Vec<usize> = (0..contingencies.len()).collect();
            return (all_critical, 0);
        }
    };
    let mut lodf_columns = model.lodf_columns();

    let mut critical = Vec::new();
    let mut screened = 0usize;

    for (ctg_idx, contingency) in contingencies.iter().enumerate() {
        if contingency.branch_indices.len() != 1
            || !contingency.generator_indices.is_empty()
            || !contingency.hvdc_converter_indices.is_empty()
            || !contingency.hvdc_cable_indices.is_empty()
            || !contingency.switch_ids.is_empty()
            || !contingency.modifications.is_empty()
        {
            // LODF screening is only valid for single-branch outages.  Multi-
            // outage contingencies are left for the exact AC path or the
            // dedicated N-2 engine so we never silently reuse single-outage
            // logic on them.
            critical.push(ctg_idx);
            continue;
        }

        let mut is_critical = false;

        for &outaged_br in &contingency.branch_indices {
            if outaged_br >= n_br {
                continue;
            }
            let branch_k = &network.branches[outaged_br];
            if !branch_k.in_service || branch_k.x.abs() < 1e-20 {
                continue;
            }

            let lodf_column = match lodf_columns.compute_column(&all_branches, outaged_br) {
                Ok(column) => column,
                Err(e) => {
                    warn!("LODF column solve failed for outage branch {outaged_br}: {e}");
                    is_critical = true;
                    break;
                }
            };

            if lodf_column[outaged_br].is_infinite() || lodf_column[outaged_br].is_nan() {
                is_critical = true;
                break;
            }

            let base_flow_k = base_dc_flows[outaged_br];
            for (branch_idx, branch_l) in network.branches.iter().enumerate() {
                let rating_l = get_rating(branch_l, thermal_rating);
                if !branch_l.in_service
                    || rating_l <= 0.0
                    || branch_idx == outaged_br
                    || branch_l.x.abs() < 1e-20
                {
                    continue;
                }

                let lodf_lk = lodf_column[branch_idx];
                if !lodf_lk.is_finite() {
                    is_critical = true;
                    break;
                }

                let post_flow = base_dc_flows[branch_idx] + lodf_lk * base_flow_k;
                let loading_pct = post_flow.abs() * base_mva / rating_l * 100.0;
                if loading_pct > screening_pct {
                    is_critical = true;
                    break;
                }
            }

            if is_critical {
                break;
            }
        }

        if is_critical {
            critical.push(ctg_idx);
        } else {
            screened += 1;
        }
    }

    (critical, screened)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{data_available, load_case};
    use surge_network::network::Contingency;

    #[test]
    fn test_compute_lodf_pairs_matches_dense_case14() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case14");
        let all_branches: Vec<usize> = (0..net.n_branches()).collect();
        let lodf_dense = surge_dc::compute_lodf_matrix(
            &net,
            &surge_dc::LodfMatrixRequest::for_branches(&all_branches),
        )
        .unwrap();
        let lodf_pairs = surge_dc::compute_lodf_pairs(&net, &all_branches, &all_branches).unwrap();

        for (&(monitored, outage), &value) in &lodf_pairs {
            let dense_value = lodf_dense[(monitored, outage)];
            assert!(
                (value - dense_value).abs() < 1e-8
                    || (value.is_infinite() && dense_value.is_infinite()),
                "LODF mismatch at ({monitored},{outage}): pairs={value}, dense={dense_value}"
            );
        }
        assert!(!lodf_pairs.is_empty(), "expected non-empty LODF pair map");
    }

    #[test]
    fn test_compute_lodf_pairs_selective() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let monitored = vec![0, 1, 2];
        let outages = vec![3, 4];

        let lodf = surge_dc::compute_lodf_pairs(&net, &monitored, &outages).unwrap();
        for &(monitored_idx, outage_idx) in lodf.entries().keys() {
            assert!(
                monitored.contains(&monitored_idx),
                "unexpected monitored index {monitored_idx} in result"
            );
            assert!(
                outages.contains(&outage_idx),
                "unexpected outage index {outage_idx} in result"
            );
        }
    }

    #[test]
    fn test_sparse_screening_case118() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case118");
        let contingencies = crate::generation::generate_n1_branch_contingencies(&net);
        let dc_result = surge_dc::solve_dc(&net).expect("DC solve failed");

        let (sparse_critical, sparse_screened) = screen_with_sparse_lodf(
            &net,
            &contingencies,
            &dc_result.branch_p_flow,
            80.0,
            ThermalRating::default(),
        );

        assert_eq!(
            sparse_critical.len() + sparse_screened,
            contingencies.len(),
            "critical + screened should equal total"
        );
        for &idx in &sparse_critical {
            assert!(idx < contingencies.len());
        }
    }

    #[test]
    fn test_sparse_screening_fails_closed_for_branch_pairs() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let dc_result = surge_dc::solve_dc(&net).expect("DC solve failed");
        let pair = Contingency {
            id: "n2_pair".into(),
            label: "branch pair".into(),
            branch_indices: vec![0, 1],
            ..Default::default()
        };
        let (critical, screened) = screen_with_sparse_lodf(
            &net,
            &[pair],
            &dc_result.branch_p_flow,
            80.0,
            ThermalRating::default(),
        );
        assert_eq!(critical, vec![0]);
        assert_eq!(screened, 0);
    }
}
