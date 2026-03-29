// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! NERC MOD-029/MOD-030: Available Transfer Capability (ATC) with reactive margin warning.
//!
//! Implements the NERC ATC framework (NERC Transmission Transfer Capability
//! methodology, 1995 rev. 2016):
//!
//! ```text
//! ATC = TTC − TRM − CBM − ETC
//! ```
//!
//! where:
//! - **TTC** (Total Transfer Capability) — maximum power transferable without
//!   violating thermal limits, computed via DC PTDF linear approximation.
//! - **TRM** (Transmission Reliability Margin) — deterministic capacity
//!   reserved for uncertainty (default: 5 % of TTC; user-settable).
//! - **CBM** (Capacity Benefit Margin) — capacity reserved for firm capacity
//!   service (default: 0 MW).
//! - **ETC** (Existing Transmission Commitments) — already-committed firm
//!   schedules that reduce available headroom (default: 0 MW).
//!
//! `NercAtcResult.ttc_mw` is the raw thermal headroom; `atc_mw` is the
//! derated value after TRM, CBM, and ETC are subtracted.
//!
//! # TTC Formula (per monitored branch)
//!
//! ```text
//! TTC_i = (Rating_i − Flow_i_pre) / PTDF_i        (if PTDF_i > 0)
//!       = (Rating_i + Flow_i_pre) / |PTDF_i|       (if PTDF_i < 0)
//! TTC = min(TTC_i) clamped to 0
//! ATC = max(0, TTC − TRM − CBM − ETC)
//! ```
//!
//! Flow signs follow the DC convention: positive = from → to direction.

use surge_network::Network;
use tracing::{info, warn};

use crate::dfax::PreparedTransferModel;
use crate::error::TransferError;
use crate::types::NercAtcRequest;

pub use crate::types::{AtcMargins, NercAtcResult};

// ── Implementation ────────────────────────────────────────────────────────────

/// Compute NERC ATC for a validated transfer path and study options.
///
/// # Parameters
///
/// - `network`            — the power system network.
/// - `source_bus`         — external bus number of the injection bus (source).
/// - `sink_bus`           — external bus number of the withdrawal bus (sink).
/// - `monitored_branches` — indices of branches whose thermal ratings constrain
///   the transfer.  An empty slice returns `TTC = ∞` (unconstrained).
/// - `margins`            — NERC TRM/CBM/ETC deductions. `None` uses defaults
///   (5 % TRM, 0 CBM, 0 ETC).
/// - `contingency_branches` — if `Some`, evaluate N-1 post-contingency limits
///   using LODF for each contingency branch. The binding TTC is the minimum
///   across N-0 and all N-1 contingencies. `None` skips N-1 evaluation (N-0 only).
///
/// # Errors
///
/// Returns `DcError` if:
/// - `source_bus` or `sink_bus` are not found in the network.
/// - The base-case DC power flow fails to converge.
/// - The PTDF B′ factorization fails (disconnected network).
pub fn compute_nerc_atc(
    network: &Network,
    request: &NercAtcRequest,
) -> Result<NercAtcResult, TransferError> {
    info!(
        path = %request.path.name,
        sources = request.path.source_buses.len(),
        sinks = request.path.sink_buses.len(),
        "computing NERC ATC"
    );
    let mut prepared = PreparedTransferModel::new(network)?;
    let result = prepared.compute_nerc_atc(request)?;

    if result.reactive_margin_warning {
        warn!(
            path = %request.path.name,
            "reactive margin warning — generator Q > 70% of Qmax near transfer path"
        );
    }

    info!(
        path = %request.path.name,
        atc_mw = result.atc_mw,
        ttc_mw = result.ttc_mw,
        trm_mw = result.trm_mw,
        limit_cause = %result.limit_cause,
        reactive_warning = result.reactive_margin_warning,
        "NERC ATC computed"
    );

    Ok(result)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::case_path;
    use crate::types::{AtcOptions, NercAtcRequest, TransferPath};
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    /// Build a 3-bus network in a line topology:
    ///
    /// ```text
    ///  Bus 1 (slack, gen) →[br0, rate=100 MW]→ Bus 2 →[br1, rate=200 MW]→ Bus 3 (load 100 MW)
    /// ```
    fn make_line_3bus(gen_qg_mvar: f64, gen_qmax_mvar: f64) -> Network {
        let mut net = Network::new("nerc_atc_line_3bus");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 138.0));

        let mut bus2 = Bus::new(2, BusType::PQ, 138.0);
        bus2.voltage_magnitude_pu = 1.0;
        net.buses.push(bus2);

        let mut bus3 = Bus::new(3, BusType::PQ, 138.0);
        bus3.voltage_magnitude_pu = 1.0;
        net.buses.push(bus3);
        net.loads.push(Load::new(3, 100.0, 0.0)); // 100 MW load

        let mut generator = Generator::new(1, 1.5, 1.0); // 150 MW at bus 1
        generator.q = gen_qg_mvar / 100.0; // convert MVAr to p.u.
        generator.qmax = gen_qmax_mvar / 100.0;
        generator.qmin = -(gen_qmax_mvar / 100.0);
        net.generators.push(generator);

        // Branch 0: bus1 → bus2, tight rating 100 MW
        let mut br0 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br0.rating_a_mva = 100.0;
        br0.rating_b_mva = 100.0;
        br0.rating_c_mva = 100.0;
        net.branches.push(br0);

        // Branch 1: bus2 → bus3, generous rating 200 MW
        let mut br1 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br1.rating_a_mva = 200.0;
        br1.rating_b_mva = 200.0;
        br1.rating_c_mva = 200.0;
        net.branches.push(br1);

        net
    }

    /// Build a 3-bus triangular network where a non-monitored outage changes
    /// the transfer sensitivity on the monitored path.
    fn make_triangle_3bus() -> Network {
        let mut net = Network::new("nerc_atc_triangle_3bus");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));

        let bus3 = Bus::new(3, BusType::PQ, 138.0);
        net.buses.push(bus3);
        net.loads.push(Load::new(3, 100.0, 0.0));

        let generator = Generator::new(1, 2.0, 0.0);
        net.generators.push(generator);

        let mut br0 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br0.rating_a_mva = 110.0;
        br0.rating_b_mva = 110.0;
        br0.rating_c_mva = 110.0;
        net.branches.push(br0);

        let mut br1 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br1.rating_a_mva = 200.0;
        br1.rating_b_mva = 200.0;
        br1.rating_c_mva = 200.0;
        net.branches.push(br1);

        let mut br2 = Branch::new_line(1, 3, 0.0, 0.2, 0.0);
        br2.rating_a_mva = 200.0;
        br2.rating_b_mva = 200.0;
        br2.rating_c_mva = 200.0;
        net.branches.push(br2);

        net
    }

    fn atc_request(
        source_bus: u32,
        sink_bus: u32,
        monitored_branches: Option<Vec<usize>>,
        margins: AtcMargins,
        contingency_branches: Option<Vec<usize>>,
    ) -> NercAtcRequest {
        NercAtcRequest {
            path: TransferPath::new("test_path", vec![source_bus], vec![sink_bus]),
            options: AtcOptions {
                monitored_branches,
                contingency_branches,
                margins,
            },
        }
    }

    /// MOD-029-A: ATC must not exceed the binding branch rating.
    #[test]
    fn test_nerc_atc_le_rating() {
        let net = make_line_3bus(0.0, 100.0);
        let monitored = vec![0usize, 1];

        let result = compute_nerc_atc(
            &net,
            &atc_request(1, 3, Some(monitored.clone()), AtcMargins::default(), None),
        )
        .expect("ATC computation should succeed");

        let max_rating = net
            .branches
            .iter()
            .map(|b| b.rating_a_mva)
            .fold(f64::NEG_INFINITY, f64::max);

        assert!(
            result.atc_mw <= max_rating,
            "ATC {} must not exceed max rating {}",
            result.atc_mw,
            max_rating
        );
    }

    /// MOD-029-B: ATC is non-negative for an underloaded network.
    #[test]
    fn test_nerc_atc_non_negative() {
        let net = make_line_3bus(0.0, 100.0);
        let monitored = vec![0usize, 1];

        let result = compute_nerc_atc(
            &net,
            &atc_request(1, 3, Some(monitored.clone()), AtcMargins::default(), None),
        )
        .expect("ATC computation should succeed");

        assert!(
            result.atc_mw >= 0.0,
            "ATC must be non-negative, got {}",
            result.atc_mw
        );
    }

    /// MOD-029-C: transfer_ptdf length equals monitored_branches length.
    #[test]
    fn test_nerc_transfer_ptdf_length() {
        let net = make_line_3bus(0.0, 100.0);
        let monitored = vec![0usize, 1];

        let result = compute_nerc_atc(
            &net,
            &atc_request(1, 3, Some(monitored.clone()), AtcMargins::default(), None),
        )
        .expect("ATC computation should succeed");

        assert_eq!(
            result.transfer_ptdf.len(),
            monitored.len(),
            "transfer_ptdf length should equal monitored_branches length"
        );
    }

    /// MOD-029-D: reactive_margin_warning = false when generator Q is well within limits.
    #[test]
    fn test_nerc_no_reactive_warning_low_q() {
        // Generator at bus 1 has qg = 10 MVAr, qmax = 100 MVAr → 10 % < 70 %.
        let net = make_line_3bus(10.0, 100.0);
        let monitored = vec![0usize, 1];

        let result = compute_nerc_atc(
            &net,
            &atc_request(1, 3, Some(monitored.clone()), AtcMargins::default(), None),
        )
        .expect("ATC computation should succeed");

        assert!(
            !result.reactive_margin_warning,
            "expected no reactive margin warning when Q = 10 % of Qmax"
        );
    }

    /// MOD-029-E: reactive_margin_warning = true when generator Q exceeds 70% of Qmax.
    #[test]
    fn test_nerc_reactive_warning_high_q() {
        // Generator at bus 1 has qg = 80 MVAr, qmax = 100 MVAr → 80 % > 70 %.
        let net = make_line_3bus(80.0, 100.0);
        let monitored = vec![0usize, 1];

        let result = compute_nerc_atc(
            &net,
            &atc_request(1, 3, Some(monitored.clone()), AtcMargins::default(), None),
        )
        .expect("ATC computation should succeed");

        assert!(
            result.reactive_margin_warning,
            "expected reactive margin warning when |Q| = 80 % of Qmax"
        );
    }

    /// MOD-029-F: invalid bus number returns Err.
    #[test]
    fn test_nerc_invalid_bus_returns_err() {
        let net = make_line_3bus(0.0, 100.0);
        let result = compute_nerc_atc(
            &net,
            &atc_request(999, 3, Some(vec![0, 1]), AtcMargins::default(), None),
        );
        assert!(
            result.is_err(),
            "expected Err for non-existent source_bus 999"
        );
    }

    /// MOD-029-G: ATC with empty monitored branches is unconstrained (infinity).
    #[test]
    fn test_nerc_empty_monitored_unconstrained() {
        let net = make_line_3bus(0.0, 100.0);
        let result = compute_nerc_atc(
            &net,
            &atc_request(1, 3, Some(vec![]), AtcMargins::default(), None),
        )
        .expect("ATC computation should succeed");
        assert!(
            result.atc_mw.is_infinite(),
            "ATC should be infinite with no monitored branches, got {}",
            result.atc_mw
        );
    }

    /// MOD-029-H: case9 integration test — runs without panic, limit metadata
    /// stays valid, and transfer_ptdf has correct length.
    #[test]
    fn test_nerc_case9() {
        let net = surge_io::load(case_path("case9")).expect("parse case9");

        let monitored: Vec<usize> = (0..net.n_branches()).collect();
        let result = compute_nerc_atc(
            &net,
            &atc_request(1, 9, Some(monitored.clone()), AtcMargins::default(), None),
        )
        .expect("ATC on case9 should succeed");

        assert!(result.atc_mw >= 0.0, "ATC must be non-negative");
        if let Some(bb) = result.binding_branch() {
            assert!(bb < net.n_branches(), "binding_branch index must be valid");
        }
        assert_eq!(result.transfer_ptdf.len(), monitored.len());
        // N-0 only → binding_contingency should be None.
        assert_eq!(result.binding_contingency(), None);
    }

    /// MOD-029-I: N-1 ATC must be ≤ N-0 ATC (post-contingency is more constrained).
    #[test]
    fn test_nerc_n1_atc_le_n0_atc() {
        let net = surge_io::load(case_path("case9")).expect("parse case9");

        let monitored: Vec<usize> = (0..net.n_branches()).collect();
        let ctg_branches: Vec<usize> = (0..net.n_branches()).collect();

        let n0 = compute_nerc_atc(
            &net,
            &atc_request(1, 9, Some(monitored.clone()), AtcMargins::default(), None),
        )
        .expect("N-0 ATC should succeed");
        let n1 = compute_nerc_atc(
            &net,
            &atc_request(
                1,
                9,
                Some(monitored.clone()),
                AtcMargins::default(),
                Some(ctg_branches.clone()),
            ),
        )
        .expect("N-1 ATC should succeed");

        assert!(
            n1.atc_mw <= n0.atc_mw + 1e-6,
            "N-1 ATC ({:.2}) must be ≤ N-0 ATC ({:.2})",
            n1.atc_mw,
            n0.atc_mw,
        );
        assert!(n1.atc_mw >= 0.0, "N-1 ATC must be non-negative");
        // If N-1 is more binding than N-0, binding_contingency should be Some.
        if n1.atc_mw < n0.atc_mw - 1e-6 {
            assert!(
                n1.binding_contingency().is_some(),
                "N-1 more binding than N-0 → binding_contingency must be Some"
            );
        }
    }

    /// MOD-029-J: bridge outages must fail closed instead of being skipped.
    #[test]
    fn test_nerc_n1_bridge_outage_fails_closed() {
        let net = make_line_3bus(0.0, 100.0);
        let mut net = net;
        net.branches[0].rating_a_mva = 200.0;
        net.branches[0].rating_b_mva = 200.0;
        net.branches[0].rating_c_mva = 200.0;
        net.branches[1].rating_a_mva = 200.0;
        net.branches[1].rating_b_mva = 200.0;
        net.branches[1].rating_c_mva = 200.0;
        let monitored = vec![0usize, 1];
        let contingency = vec![0usize];

        let result = compute_nerc_atc(
            &net,
            &atc_request(
                1,
                3,
                Some(monitored),
                AtcMargins::default(),
                Some(contingency),
            ),
        )
        .expect("ATC computation should succeed");

        assert_eq!(
            result.atc_mw, 0.0,
            "bridge outage should fail closed to zero ATC, got {}",
            result.atc_mw
        );
        assert_eq!(
            result.ttc_mw, 0.0,
            "bridge outage should fail closed to zero TTC, got {}",
            result.ttc_mw
        );
        assert_eq!(
            result.binding_contingency(),
            Some(0),
            "bridge outage should identify the binding contingency"
        );
        assert_eq!(
            result.binding_branch(),
            None,
            "fail-closed outage should not invent a monitored binding branch"
        );
    }

    #[test]
    fn test_nerc_n1_atc_uses_contingency_transfer_ptdf_for_unmonitored_outage() {
        let net = make_triangle_3bus();
        let monitored = vec![0usize];
        let contingency = vec![2usize];
        let zero_margins = AtcMargins {
            trm_fraction: 0.0,
            cbm_mw: 0.0,
            etc_mw: 0.0,
        };

        let result = compute_nerc_atc(
            &net,
            &atc_request(
                1,
                3,
                Some(monitored.clone()),
                zero_margins,
                Some(contingency.clone()),
            ),
        )
        .expect("N-1 ATC should succeed");

        let mut model = surge_dc::PreparedDcStudy::new(&net).expect("prepared dc model");
        let dc = model
            .solve(&surge_dc::DcPfOptions::default())
            .expect("base dc solve");
        let ptdf = model.compute_ptdf(&[0, 2]).expect("ptdf rows");
        let lodf = model
            .compute_lodf(&monitored, &contingency)
            .expect("subset lodf");
        let bus_map = net.bus_index_map();
        let source_idx = *bus_map.get(&1).expect("source bus");
        let sink_idx = *bus_map.get(&3).expect("sink bus");

        let branch_transfer_ptdf = |branch_idx: usize| -> f64 {
            let row = ptdf.row(branch_idx).expect("ptdf row");
            row[source_idx] - row[sink_idx]
        };
        let base_flows_mw: Vec<f64> = dc.branch_p_flow.iter().map(|&f| f * net.base_mva).collect();
        let lodf_02 = lodf[(0, 0)];
        assert!(
            lodf_02.is_finite(),
            "triangle outage should remain connected and yield finite LODF"
        );

        let post_flow = base_flows_mw[0] + lodf_02 * base_flows_mw[2];
        let post_np = branch_transfer_ptdf(0) + lodf_02 * branch_transfer_ptdf(2);
        let expected_ttc = ((net.branches[0].rating_a_mva - post_flow) / post_np).max(0.0);

        assert!(
            (result.ttc_mw - expected_ttc).abs() < 1e-9,
            "expected TTC {expected_ttc}, got {}",
            result.ttc_mw
        );
        assert_eq!(result.binding_branch(), Some(0));
        assert_eq!(result.binding_contingency(), Some(2));
    }
}
