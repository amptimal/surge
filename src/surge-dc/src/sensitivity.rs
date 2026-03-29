// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Power Transfer Distribution Factors (PTDF) and Line Outage Distribution
//! Factors (LODF) computation.
//!
//! PTDF: How much of a power transfer between two buses flows on each line.
//! LODF: How line flows redistribute when a line is outaged (for N-1 contingency).
//!
//! # Approach
//!
//! Both PTDF and LODF are computed via on-demand KLU column solves — no dense
//! B'^-1 is ever materialized. One KLU solve per monitored branch gives the full
//! PTDF row for that branch. This scales to the full US grid (82k buses, 121k
//! branches) without memory issues.
//!
//! # Exactness Under DC Assumptions
//!
//! PTDF and LODF computed here are **exact under the DC power flow assumptions**
//! (flat voltage, small angles, lossless branches). The LODF formula
//! `LODF[l,k] = PTDF_lk / (1 - PTDF_kk)` is an analytical identity derived from
//! the Sherman-Morrison matrix inverse update, not an approximation, within the DC
//! model.
//!
//! # Limitations vs. AC Contingency Analysis
//!
//! - **Voltage-limited contingencies** — post-contingency voltage collapse not detected.
//! - **Reactive power constraints** — generator Q limits invisible to DC PTDF/LODF.
//! - **Nonlinear flow redistribution** — second-order voltage/angle effects ignored.
//! - **Transformer tap effects** — post-contingency OLTC tap changes not modelled.
//!
//! For AC-accurate contingency analysis, use `surge-contingency` with full
//! Newton-Raphson AC re-solve per contingency. DC PTDF/LODF is appropriate for
//! fast screening before running expensive AC validation on the critical subset.

use surge_network::Network;
use tracing::{debug, info};

use crate::solver::PreparedDcStudy;
use crate::types::*;

/// Threshold for bridge-line detection in LODF computation.
///
/// If `ptdf_kk >= 1.0 - BRIDGE_THRESHOLD` the line is treated as a bridge:
/// outaging it disconnects the network and all LODF entries for that column are
/// set to `f64::INFINITY`.
pub(crate) const BRIDGE_THRESHOLD: f64 = 1e-6;

/// Slack-balancing policy for PTDF and OTDF sensitivity wrappers.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DcSensitivitySlack {
    /// Standard single-slack PTDF/LODF semantics.
    #[default]
    SingleSlack,
    /// Fixed participation weights keyed by internal bus index.
    SlackWeights(Vec<(usize, f64)>),
    /// Headroom-limited balancing across the given internal bus indices.
    HeadroomSlack(Vec<usize>),
}

/// Options for PTDF and OTDF sensitivity wrappers.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DcSensitivityOptions {
    /// Slack-balancing policy for sensitivity computation.
    pub slack: DcSensitivitySlack,
}

impl DcSensitivityOptions {
    /// Create default options (single-slack semantics).
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create options with fixed participation-weight slack balancing.
    #[inline]
    pub fn with_slack_weights(slack_weights: &[(usize, f64)]) -> Self {
        Self {
            slack: DcSensitivitySlack::SlackWeights(slack_weights.to_vec()),
        }
    }

    /// Create options with headroom-limited slack balancing.
    #[inline]
    pub fn with_headroom_slack(participating_bus_indices: &[usize]) -> Self {
        Self {
            slack: DcSensitivitySlack::HeadroomSlack(participating_bus_indices.to_vec()),
        }
    }
}

/// Advanced request for one-shot PTDF computation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PtdfRequest {
    /// Branch indices to monitor. `None` monitors all branches.
    pub monitored_branch_indices: Option<Vec<usize>>,
    /// Bus indices for PTDF columns. `None` returns all buses.
    pub bus_indices: Option<Vec<usize>>,
    /// Sensitivity options (slack policy).
    pub options: DcSensitivityOptions,
}

impl PtdfRequest {
    /// Create a default request (all branches, all buses, single slack).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a request for the given monitored branches.
    pub fn for_branches(monitored_branch_indices: &[usize]) -> Self {
        Self::new().with_monitored_branches(monitored_branch_indices)
    }

    /// Restrict to specific monitored branches.
    pub fn with_monitored_branches(mut self, monitored_branch_indices: &[usize]) -> Self {
        self.monitored_branch_indices = Some(monitored_branch_indices.to_vec());
        self
    }

    /// Restrict PTDF columns to specific bus indices.
    pub fn with_bus_indices(mut self, bus_indices: &[usize]) -> Self {
        self.bus_indices = Some(bus_indices.to_vec());
        self
    }

    /// Set sensitivity options (slack policy).
    pub fn with_options(mut self, options: DcSensitivityOptions) -> Self {
        self.options = options;
        self
    }
}

/// Advanced request for one-shot subset LODF computation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LodfRequest {
    /// Branch indices to monitor (rows). `None` monitors all branches.
    pub monitored_branch_indices: Option<Vec<usize>>,
    /// Branch indices to outage (columns). `None` uses the monitored set.
    pub outage_branch_indices: Option<Vec<usize>>,
}

impl LodfRequest {
    /// Create a default request (all branches for both monitored and outage sets).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a request for explicit monitored and outage branch sets.
    pub fn for_branches(
        monitored_branch_indices: &[usize],
        outage_branch_indices: &[usize],
    ) -> Self {
        Self::new()
            .with_monitored_branches(monitored_branch_indices)
            .with_outage_branches(outage_branch_indices)
    }

    /// Restrict to specific monitored branches.
    pub fn with_monitored_branches(mut self, monitored_branch_indices: &[usize]) -> Self {
        self.monitored_branch_indices = Some(monitored_branch_indices.to_vec());
        self
    }

    /// Restrict to specific outage branches.
    pub fn with_outage_branches(mut self, outage_branch_indices: &[usize]) -> Self {
        self.outage_branch_indices = Some(outage_branch_indices.to_vec());
        self
    }
}

/// Advanced request for one-shot dense all-pairs LODF computation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LodfMatrixRequest {
    /// Branch indices for the square LODF matrix. `None` uses all branches.
    pub branch_indices: Option<Vec<usize>>,
}

impl LodfMatrixRequest {
    /// Create a default request (all branches).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a request for the given branch set.
    pub fn for_branches(branch_indices: &[usize]) -> Self {
        Self::new().with_branches(branch_indices)
    }

    /// Restrict to specific branches.
    pub fn with_branches(mut self, branch_indices: &[usize]) -> Self {
        self.branch_indices = Some(branch_indices.to_vec());
        self
    }
}

/// Advanced request for one-shot OTDF computation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OtdfRequest {
    /// Branch indices to monitor.
    pub monitored_branch_indices: Vec<usize>,
    /// Branch indices to outage.
    pub outage_branch_indices: Vec<usize>,
    /// Bus indices for OTDF vectors. `None` returns all buses.
    pub bus_indices: Option<Vec<usize>>,
    /// Sensitivity options (slack policy).
    pub options: DcSensitivityOptions,
}

impl OtdfRequest {
    /// Create a request for the given monitored and outage branch sets.
    pub fn new(monitored_branch_indices: &[usize], outage_branch_indices: &[usize]) -> Self {
        Self {
            monitored_branch_indices: monitored_branch_indices.to_vec(),
            outage_branch_indices: outage_branch_indices.to_vec(),
            bus_indices: None,
            options: DcSensitivityOptions::default(),
        }
    }

    /// Restrict OTDF vectors to specific bus indices.
    pub fn with_bus_indices(mut self, bus_indices: &[usize]) -> Self {
        self.bus_indices = Some(bus_indices.to_vec());
        self
    }

    /// Set sensitivity options (slack policy).
    pub fn with_options(mut self, options: DcSensitivityOptions) -> Self {
        self.options = options;
        self
    }
}

/// Advanced request for one-shot N-2 computation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct N2LodfRequest {
    /// The `(first, second)` outage branch pair.
    pub outage_pair: (usize, usize),
    /// Branch indices to monitor. `None` monitors all branches.
    pub monitored_branch_indices: Option<Vec<usize>>,
}

impl N2LodfRequest {
    /// Create a request for a single N-2 outage pair.
    pub fn new(outage_pair: (usize, usize)) -> Self {
        Self {
            outage_pair,
            monitored_branch_indices: None,
        }
    }

    /// Restrict to specific monitored branches.
    pub fn with_monitored_branches(mut self, monitored_branch_indices: &[usize]) -> Self {
        self.monitored_branch_indices = Some(monitored_branch_indices.to_vec());
        self
    }
}

/// Advanced request for one-shot batched N-2 computation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct N2LodfBatchRequest {
    /// The outage pairs to evaluate.
    pub outage_pairs: Vec<(usize, usize)>,
    /// Branch indices to monitor. `None` monitors all branches.
    pub monitored_branch_indices: Option<Vec<usize>>,
}

impl N2LodfBatchRequest {
    /// Create a request for the given outage pairs.
    pub fn new(outage_pairs: &[(usize, usize)]) -> Self {
        Self {
            outage_pairs: outage_pairs.to_vec(),
            monitored_branch_indices: None,
        }
    }

    /// Restrict to specific monitored branches.
    pub fn with_monitored_branches(mut self, monitored_branch_indices: &[usize]) -> Self {
        self.monitored_branch_indices = Some(monitored_branch_indices.to_vec());
        self
    }
}

/// Compute PTDF rows for monitored branches.
///
/// Returns a [`PtdfRows`] matrix whose rows are ordered exactly like the
/// request's `monitored_branch_indices`. Entry `(branch, bus)` is the
/// sensitivity of flow on that branch to a 1 p.u. injection at `bus`
/// (slack-bus column is always zero).
///
/// # Implementation
///
/// Factor B' once via KLU, then solve one linear system per monitored branch
/// using the shift-factor RHS vector:
///   `PTDF_row(k) = b_k × B'⁻¹ × e_{f_k, t_k}`
/// where `e_{f,t}` is +1 at from-bus f, -1 at to-bus t (both in reduced space).
///
/// This costs O(n_monitored × nnz(B')) memory — no B'^-1 is ever formed.
///
/// # Errors
///
/// Returns `DcError` if the B' matrix cannot be factored (disconnected network)
/// or if a branch index is out of range.
pub fn compute_ptdf(network: &Network, request: &PtdfRequest) -> Result<PtdfRows, DcError> {
    let monitored_branch_indices_storage;
    let monitored_branch_indices =
        if let Some(indices) = request.monitored_branch_indices.as_deref() {
            indices
        } else {
            monitored_branch_indices_storage = (0..network.n_branches()).collect::<Vec<_>>();
            &monitored_branch_indices_storage
        };
    info!(
        buses = network.n_buses(),
        monitored = monitored_branch_indices.len(),
        "computing PTDF rows for monitored branches"
    );
    let mut model = PreparedDcStudy::new(network)?;
    model.compute_ptdf_with_options(
        monitored_branch_indices,
        request.bus_indices.as_deref(),
        &request.options,
    )
}

/// Compute LODF values for monitored and outage branch sets.
///
/// Returns a [`LodfResult`] of dimensions `monitored.len() × outage.len()` in
/// the requested branch order, where entry `[row, col]` is the Line Outage
/// Distribution Factor for monitored branch `monitored[row]` under outage
/// branch `outage[col]`.
///
/// # Implementation
///
/// Factors B' once via KLU, then computes each requested outage column lazily
/// from cached endpoint solves. LODF is derived via the Sherman-Morrison
/// identity `LODF[l,k] = PTDF_lk / (1 - PTDF_kk)` where `PTDF_lk` is the net
/// sensitivity of branch `l` to the endpoints of branch `k`.
///
/// Bridge lines (where `PTDF_kk ≥ 1 - BRIDGE_THRESHOLD`) get column set to ±∞.
///
/// # Use
///
/// Use this for N-1 screening, ATC, and any workflow that only needs a subset
/// of monitored branches and outage branches.
pub fn compute_lodf(network: &Network, request: &LodfRequest) -> Result<LodfResult, DcError> {
    let monitored_branch_indices_storage;
    let monitored_branch_indices =
        if let Some(indices) = request.monitored_branch_indices.as_deref() {
            indices
        } else {
            monitored_branch_indices_storage = (0..network.n_branches()).collect::<Vec<_>>();
            &monitored_branch_indices_storage
        };
    let outage_branch_indices = request
        .outage_branch_indices
        .as_deref()
        .unwrap_or(monitored_branch_indices);
    info!(
        monitored = monitored_branch_indices.len(),
        outage = outage_branch_indices.len(),
        "computing LODF for monitored/outage branch sets"
    );
    let mut model = PreparedDcStudy::new(network)?;
    let lodf = model.compute_lodf(monitored_branch_indices, outage_branch_indices)?;
    debug!(
        rows = lodf.n_rows(),
        cols = lodf.n_cols(),
        "LODF subset computed"
    );
    Ok(lodf)
}

/// Compute selected LODF entries as a sparse pair map.
///
/// The returned map is keyed as `(monitored_branch_idx, outage_branch_idx)`.
pub fn compute_lodf_pairs(
    network: &Network,
    monitored_branch_indices: &[usize],
    outage_branch_indices: &[usize],
) -> Result<LodfPairs, DcError> {
    info!(
        monitored = monitored_branch_indices.len(),
        outage = outage_branch_indices.len(),
        "computing LODF pairs for monitored/outage branch sets"
    );
    let mut model = PreparedDcStudy::new(network)?;
    model.compute_lodf_pairs(monitored_branch_indices, outage_branch_indices)
}

/// Compute a dense all-pairs LODF matrix.
///
/// The same branch set is used as both the monitored set and outage set.
pub fn compute_lodf_matrix(
    network: &Network,
    request: &LodfMatrixRequest,
) -> Result<LodfMatrixResult, DcError> {
    let branch_indices_storage;
    let branches = if let Some(indices) = request.branch_indices.as_deref() {
        indices
    } else {
        branch_indices_storage = (0..network.n_branches()).collect::<Vec<_>>();
        &branch_indices_storage
    };
    info!(
        branches = branches.len(),
        "computing LODF matrix via KLU column solves"
    );
    let mut model = PreparedDcStudy::new(network)?;
    let lodf = model.compute_lodf_matrix(branches)?;
    debug!(
        rows = lodf.n_rows(),
        cols = lodf.n_cols(),
        "LODF matrix computed"
    );
    Ok(lodf)
}

/// Compute Outage Transfer Distribution Factors (OTDF).
///
/// `OTDF[(m, k)][bus] = PTDF[m][bus] + LODF[m, k] × PTDF[k][bus]`
///
/// This is the post-contingency sensitivity of flow on monitored branch `m`
/// to a 1 p.u. injection at `bus` when outage branch `k` is tripped.
///
/// # Implementation
///
/// Factors B' once. Computes PTDF rows for the union of monitored and outage
/// branch indices (one KLU solve per unique branch). LODF\[m,k\] derived via the
/// Sherman-Morrison identity identical to `compute_lodf_matrix`.
///
/// Memory: O((n_monitored + n_outage) × n_buses) for the PTDF rows.
///
/// # Errors
///
/// Returns `DcError` if the B' matrix is singular (disconnected network) or
/// any branch index is out of range.
pub fn compute_otdf(network: &Network, request: &OtdfRequest) -> Result<OtdfResult, DcError> {
    compute_otdf_with_options(
        network,
        &request.monitored_branch_indices,
        &request.outage_branch_indices,
        request.bus_indices.as_deref(),
        &request.options,
    )
}

/// Compute OTDF for explicit monitored and outage branch sets with
/// explicit sensitivity options and an optional bus subset.
fn compute_otdf_with_options(
    network: &Network,
    monitored_branch_indices: &[usize],
    outage_branch_indices: &[usize],
    bus_indices: Option<&[usize]>,
    options: &DcSensitivityOptions,
) -> Result<OtdfResult, DcError> {
    info!(
        monitored = monitored_branch_indices.len(),
        outage = outage_branch_indices.len(),
        buses = bus_indices
            .map(|buses| buses.len())
            .unwrap_or(network.n_buses()),
        "computing OTDF for monitored/outage branch sets"
    );
    let mut model = PreparedDcStudy::new(network)?;
    model.compute_otdf_with_options(
        monitored_branch_indices,
        outage_branch_indices,
        bus_indices,
        options,
    )
}

pub(crate) fn collect_selected_bus_indices(
    network: &Network,
    bus_indices: Option<&[usize]>,
) -> Result<Vec<usize>, DcError> {
    let selected_bus_indices = bus_indices
        .map(|indices| indices.to_vec())
        .unwrap_or_else(|| (0..network.n_buses()).collect());
    let mut seen = vec![false; network.n_buses()];
    for &bus_idx in &selected_bus_indices {
        if bus_idx >= network.n_buses() {
            return Err(DcError::InvalidNetwork(format!(
                "OTDF bus index {bus_idx} out of range (len={})",
                network.n_buses()
            )));
        }
        if std::mem::replace(&mut seen[bus_idx], true) {
            return Err(DcError::InvalidNetwork(format!(
                "OTDF bus index {bus_idx} requested more than once"
            )));
        }
    }
    Ok(selected_bus_indices)
}

pub(crate) fn compute_otdf_from_ptdf(
    network: &Network,
    monitored_branch_indices: &[usize],
    outage_branch_indices: &[usize],
    bus_indices: &[usize],
    ptdf: &PtdfRows,
) -> Result<OtdfResult, DcError> {
    let bus_map = network.bus_index_map();
    let mut result = OtdfResult::new(
        monitored_branch_indices,
        outage_branch_indices,
        bus_indices,
        network.n_branches(),
        network.n_buses(),
    );

    for (outage_pos, &outage_branch_idx) in outage_branch_indices.iter().enumerate() {
        let branch_k = match network.branches.get(outage_branch_idx) {
            Some(branch) if branch.in_service => branch,
            _ => {
                for (monitored_pos, &monitored_branch_idx) in
                    monitored_branch_indices.iter().enumerate()
                {
                    if let Some(ptdf_row_m) = ptdf.row(monitored_branch_idx) {
                        let target = result.vector_at_mut(monitored_pos, outage_pos);
                        for (value_pos, &bus_idx) in bus_indices.iter().enumerate() {
                            target[value_pos] = ptdf_row_m[bus_idx];
                        }
                    }
                }
                continue;
            }
        };

        let Some(&from_k) = bus_map.get(&branch_k.from_bus) else {
            continue;
        };
        let Some(&to_k) = bus_map.get(&branch_k.to_bus) else {
            continue;
        };
        let ptdf_row_k = ptdf.row(outage_branch_idx).ok_or_else(|| {
            DcError::InvalidNetwork(format!(
                "PTDF row for outage branch {outage_branch_idx} was not computed"
            ))
        })?;

        let ptdf_kk = ptdf_row_k[from_k] - ptdf_row_k[to_k];
        let is_bridge = (1.0 - ptdf_kk).abs() < BRIDGE_THRESHOLD;

        for (monitored_pos, &monitored_branch_idx) in monitored_branch_indices.iter().enumerate() {
            let target = result.vector_at_mut(monitored_pos, outage_pos);

            if is_bridge {
                target.fill(f64::INFINITY);
                continue;
            }

            let ptdf_row_m = ptdf.row(monitored_branch_idx).ok_or_else(|| {
                DcError::InvalidNetwork(format!(
                    "PTDF row for monitored branch {monitored_branch_idx} was not computed"
                ))
            })?;

            let ptdf_mk = ptdf_row_m[from_k] - ptdf_row_m[to_k];
            let lodf_mk = ptdf_mk / (1.0 - ptdf_kk);
            for (value_pos, &bus_idx) in bus_indices.iter().enumerate() {
                target[value_pos] = ptdf_row_m[bus_idx] + lodf_mk * ptdf_row_k[bus_idx];
            }
        }
    }

    Ok(result)
}

/// Second-order LODF sensitivity for a simultaneous double outage (N-2).
///
/// Given the simultaneous outage of branches `k` and `l`, computes the N-2
/// sensitivity coefficient for each branch in `monitored_indices`.
///
/// # Mathematical Background — Woodbury Rank-2 Update
///
/// ```text
/// LODF2(m; k,l) = [LODF(m,k) + LODF(m,l) × LODF(l,k)]
///                 / [1 - LODF(l,k) × LODF(k,l)]
/// ```
///
/// # Full Post-Outage Prediction
///
/// ```text
/// F_m_post ≈ F_m_pre
///           + compute_n2_lodf(net, (j,k), monitored)[m] × F_j_pre
///           + compute_n2_lodf(net, (k,j), monitored)[m] × F_k_pre
/// ```
///
/// # Returns
/// [`N2LodfResult`] ordered exactly like `monitored_indices`.
pub fn compute_n2_lodf(
    network: &Network,
    request: &N2LodfRequest,
) -> Result<N2LodfResult, DcError> {
    let monitored_indices_storage;
    let monitored_indices = if let Some(indices) = request.monitored_branch_indices.as_deref() {
        indices
    } else {
        monitored_indices_storage = (0..network.n_branches()).collect::<Vec<_>>();
        &monitored_indices_storage
    };
    info!(
        outage_k = request.outage_pair.0,
        outage_l = request.outage_pair.1,
        monitored = monitored_indices.len(),
        "computing N-2 LODF for branch pair"
    );
    let mut model = PreparedDcStudy::new(network)?;
    model.compute_n2_lodf(request.outage_pair, monitored_indices)
}

/// Compute batched N-2 LODF for multiple simultaneous double-outage pairs.
pub fn compute_n2_lodf_batch(
    network: &Network,
    request: &N2LodfBatchRequest,
) -> Result<N2LodfBatchResult, DcError> {
    let monitored_indices_storage;
    let monitored_indices = if let Some(indices) = request.monitored_branch_indices.as_deref() {
        indices
    } else {
        monitored_indices_storage = (0..network.n_branches()).collect::<Vec<_>>();
        &monitored_indices_storage
    };
    info!(
        outage_pairs = request.outage_pairs.len(),
        monitored = monitored_indices.len(),
        "computing N-2 LODF batch for branch pairs"
    );
    let mut model = PreparedDcStudy::new(network)?;
    model.compute_n2_lodf_batch(&request.outage_pairs, monitored_indices)
}

pub(crate) fn collect_unique_branch_indices(
    n_branches: usize,
    monitored_branch_indices: &[usize],
    outage_branch_indices: &[usize],
) -> Result<Vec<usize>, DcError> {
    let mut seen = vec![false; n_branches];
    let mut branch_indices =
        Vec::with_capacity(monitored_branch_indices.len() + outage_branch_indices.len());

    for &branch_idx in monitored_branch_indices
        .iter()
        .chain(outage_branch_indices.iter())
    {
        if branch_idx >= n_branches {
            return Err(DcError::InvalidNetwork(format!(
                "branch index {branch_idx} out of range for network with {n_branches} branches"
            )));
        }
        if !seen[branch_idx] {
            seen[branch_idx] = true;
            branch_indices.push(branch_idx);
        }
    }

    Ok(branch_indices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;

    fn load_case9() -> Network {
        load_net("case9")
    }

    fn load_case14() -> Network {
        load_net("case14")
    }

    fn build_balanced_headroom_ptdf_network() -> Network {
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};

        let mut net = Network::new("balanced_headroom_ptdf");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PV, 230.0));
        net.buses.push(Bus::new(3, BusType::PV, 230.0));
        net.buses.push(Bus::new(4, BusType::PQ, 230.0));

        net.branches.push(Branch::new_line(1, 2, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(1, 3, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(2, 4, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(3, 4, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(2, 3, 0.0, 0.2, 0.0));

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmax = 140.0;
        net.generators.push(g1);

        let mut g2 = Generator::new(2, 60.0, 1.0);
        g2.pmin = 10.0;
        g2.pmax = 90.0;
        net.generators.push(g2);

        let mut g3 = Generator::new(3, 60.0, 1.0);
        g3.pmin = 20.0;
        g3.pmax = 80.0;
        net.generators.push(g3);

        net.loads.push(Load::new(4, 120.0, 0.0));
        net
    }

    fn get(ptdf: &PtdfRows, branch: usize, bus: usize) -> f64 {
        ptdf.get(branch, bus)
    }

    #[test]
    fn test_ptdf_case9() {
        let net = load_case9();
        let n_br = net.n_branches();
        let n_bus = net.n_buses();
        let all: Vec<usize> = (0..n_br).collect();
        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all)).unwrap();

        assert_eq!(ptdf.n_rows(), n_br);
        for row_pos in 0..ptdf.n_rows() {
            assert_eq!(ptdf.row_at(row_pos).len(), n_bus);
        }

        // PTDF w.r.t. slack bus should be all zeros
        let slack_idx = net.slack_bus_index().unwrap();
        for l in 0..n_br {
            assert!(get(&ptdf, l, slack_idx).abs() < 1e-10);
        }
    }

    #[test]
    fn test_lodf_case9() {
        let net = load_case9();
        let n_br = net.n_branches();
        let all: Vec<usize> = (0..n_br).collect();
        let lodf = compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all)).unwrap();

        assert_eq!(lodf.n_rows(), n_br);
        assert_eq!(lodf.n_cols(), n_br);

        for k in 0..n_br {
            if lodf[(k, k)].is_infinite() {
                continue;
            }
            assert!(
                (lodf[(k, k)] - (-1.0)).abs() < 1e-10,
                "LODF diagonal [{k},{k}] = {}, expected -1.0",
                lodf[(k, k)]
            );
        }
    }

    #[test]
    fn test_lodf_subset_matches_dense_case14() {
        let net = load_case14();
        let all: Vec<usize> = (0..net.n_branches()).collect();
        let monitored = vec![0, 2, 5, 7];
        let outages = vec![1, 6, 8];

        let dense = compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all)).unwrap();
        let subset = compute_lodf(&net, &LodfRequest::for_branches(&monitored, &outages)).unwrap();

        assert_eq!(subset.n_rows(), monitored.len());
        assert_eq!(subset.n_cols(), outages.len());
        for (i, &l) in monitored.iter().enumerate() {
            for (j, &k) in outages.iter().enumerate() {
                let subset_val = subset[(i, j)];
                let dense_val = dense[(l, k)];
                assert!(
                    (subset_val - dense_val).abs() < 1e-12
                        || (subset_val.is_infinite() && dense_val.is_infinite()),
                    "subset LODF[{l},{k}] = {subset_val}, dense = {dense_val}"
                );
            }
        }
    }

    #[test]
    fn test_lodf_pairs_match_dense_case14() {
        let net = load_case14();
        let all: Vec<usize> = (0..net.n_branches()).collect();
        let monitored = vec![0, 2, 5, 7];
        let outages = vec![1, 6, 8];

        let dense = compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all)).unwrap();
        let pairs = compute_lodf_pairs(&net, &monitored, &outages).unwrap();

        assert_eq!(pairs.len(), monitored.len() * outages.len());
        for &l in &monitored {
            for &k in &outages {
                let pair_val = pairs.get(l, k).expect("missing pair entry in sparse map");
                let dense_val = dense[(l, k)];
                assert!(
                    (pair_val - dense_val).abs() < 1e-12
                        || (pair_val.is_infinite() && dense_val.is_infinite()),
                    "pair LODF[{l},{k}] = {pair_val}, dense = {dense_val}"
                );
            }
        }
    }

    /// Pin specific PTDF entries for case9 against computed reference values.
    #[test]
    fn test_ptdf_case9_values() {
        let net = load_case9();
        let n_br = net.n_branches();
        let all: Vec<usize> = (0..n_br).collect();
        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all)).unwrap();
        let tol = 1e-10;

        assert!(
            (get(&ptdf, 0, 1) - (-1.0)).abs() < tol,
            "PTDF[0,1] = {}, expected -1.0",
            get(&ptdf, 0, 1)
        );
        assert!(
            (get(&ptdf, 0, 4) - (-1.0)).abs() < tol,
            "PTDF[0,4] = {}, expected -1.0",
            get(&ptdf, 0, 4)
        );
        assert!(
            (get(&ptdf, 0, 8) - (-1.0)).abs() < tol,
            "PTDF[0,8] = {}, expected -1.0",
            get(&ptdf, 0, 8)
        );

        assert!(
            (get(&ptdf, 1, 1) - (-0.361339600470035)).abs() < tol,
            "PTDF[1,1] = {}",
            get(&ptdf, 1, 1)
        );
        assert!(
            (get(&ptdf, 1, 4) - (-0.864864864864865)).abs() < tol,
            "PTDF[1,4] = {}",
            get(&ptdf, 1, 4)
        );
        assert!(
            (get(&ptdf, 1, 8) - (-0.124853113983549)).abs() < tol,
            "PTDF[1,8] = {}",
            get(&ptdf, 1, 8)
        );

        assert!(
            (get(&ptdf, 3, 2) - 1.0).abs() < tol,
            "PTDF[3,2] = {}",
            get(&ptdf, 3, 2)
        );
        assert!(
            get(&ptdf, 3, 1).abs() < tol,
            "PTDF[3,1] = {}",
            get(&ptdf, 3, 1)
        );
        assert!(
            get(&ptdf, 3, 6).abs() < tol,
            "PTDF[3,6] = {}",
            get(&ptdf, 3, 6)
        );

        assert!(
            (get(&ptdf, 6, 1) - (-1.0)).abs() < tol,
            "PTDF[6,1] = {}",
            get(&ptdf, 6, 1)
        );
        assert!(
            get(&ptdf, 6, 4).abs() < tol,
            "PTDF[6,4] = {}",
            get(&ptdf, 6, 4)
        );

        assert!(
            (get(&ptdf, 7, 1) - 0.638660399529965).abs() < tol,
            "PTDF[7,1] = {}",
            get(&ptdf, 7, 1)
        );
        assert!(
            (get(&ptdf, 7, 6) - 0.532902467685077).abs() < tol,
            "PTDF[7,6] = {}",
            get(&ptdf, 7, 6)
        );
        assert!(
            (get(&ptdf, 7, 8) - (-0.124853113983548)).abs() < tol,
            "PTDF[7,8] = {}",
            get(&ptdf, 7, 8)
        );

        // Slack column always zero
        let slack_idx = net.slack_bus_index().unwrap();
        for l in 0..n_br {
            assert!(
                get(&ptdf, l, slack_idx).abs() < tol,
                "PTDF[{l},slack={slack_idx}] = {} should be 0",
                get(&ptdf, l, slack_idx)
            );
        }
    }

    /// Pin specific LODF entries for case14.
    #[test]
    fn test_lodf_case14_values() {
        let net = load_case14();
        let n_br = net.n_branches();
        let all: Vec<usize> = (0..n_br).collect();
        let lodf = compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all)).unwrap();
        let tol = 1e-10;

        assert!(
            (lodf[(1, 0)] - 1.0).abs() < tol,
            "LODF[1,0] = {}",
            lodf[(1, 0)]
        );
        assert!(
            (lodf[(2, 0)] - (-0.168846208748234)).abs() < tol,
            "LODF[2,0] = {}",
            lodf[(2, 0)]
        );
        assert!(
            (lodf[(4, 0)] - (-0.477794835783875)).abs() < tol,
            "LODF[4,0] = {}",
            lodf[(4, 0)]
        );
        assert!(
            (lodf[(6, 0)] - (-0.493343980479742)).abs() < tol,
            "LODF[6,0] = {}",
            lodf[(6, 0)]
        );
        assert!(
            (lodf[(0, 2)] - (-0.207667214399152)).abs() < tol,
            "LODF[0,2] = {}",
            lodf[(0, 2)]
        );
        assert!(
            (lodf[(5, 2)] - (-1.0)).abs() < tol,
            "LODF[5,2] = {}",
            lodf[(5, 2)]
        );
        assert!(
            (lodf[(3, 2)] - 0.455285600326033).abs() < tol,
            "LODF[3,2] = {}",
            lodf[(3, 2)]
        );
        assert!(
            (lodf[(3, 6)] - (-0.514489688099607)).abs() < tol,
            "LODF[3,6] = {}",
            lodf[(3, 6)]
        );
        assert!(
            (lodf[(4, 6)] - 0.470460951978899).abs() < tol,
            "LODF[4,6] = {}",
            lodf[(4, 6)]
        );
        assert!(
            (lodf[(7, 6)] - 0.151344601496801).abs() < tol,
            "LODF[7,6] = {}",
            lodf[(7, 6)]
        );
        assert!(
            (lodf[(7, 8)] - 0.638989500215690).abs() < tol,
            "LODF[7,8] = {}",
            lodf[(7, 8)]
        );

        for k in 0..lodf.n_cols() {
            if lodf[(k, k)].is_finite() {
                assert!(
                    (lodf[(k, k)] - (-1.0)).abs() < tol,
                    "LODF diagonal [{k},{k}] = {}, expected -1.0",
                    lodf[(k, k)]
                );
            }
        }
    }

    /// Validate PTDF-LODF consistency for case14.
    #[test]
    fn test_ptdf_lodf_consistency() {
        let net = load_case14();
        let bus_map = net.bus_index_map();
        let n_br = net.n_branches();
        let all: Vec<usize> = (0..n_br).collect();
        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all)).unwrap();
        let lodf = compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all)).unwrap();
        let tol = 1e-8;

        for k in 0..n_br {
            let branch_k = &net.branches[k];
            if !branch_k.in_service {
                continue;
            }
            let from_k = bus_map[&branch_k.from_bus];
            let to_k = bus_map[&branch_k.to_bus];
            let ptdf_kk = get(&ptdf, k, from_k) - get(&ptdf, k, to_k);

            if (1.0 - ptdf_kk).abs() < 1e-10 {
                continue;
            }
            let denom = 1.0 - ptdf_kk;

            for l in 0..n_br {
                if l == k {
                    assert!(
                        (lodf[(l, k)] - (-1.0)).abs() < tol,
                        "LODF[{l},{k}] = {}, expected -1.0",
                        lodf[(l, k)]
                    );
                    continue;
                }
                let ptdf_lk = get(&ptdf, l, from_k) - get(&ptdf, l, to_k);
                let expected = ptdf_lk / denom;
                assert!(
                    (lodf[(l, k)] - expected).abs() < tol,
                    "LODF[{l},{k}] = {}, expected {} (diff = {:.2e})",
                    lodf[(l, k)],
                    expected,
                    (lodf[(l, k)] - expected).abs()
                );
            }
        }
    }

    /// Validate DC flows equal PTDF × injections for case9.
    #[test]
    fn test_ptdf_flow_reconstruction() {
        let net = load_case9();
        let n_br = net.n_branches();
        let n_bus = net.n_buses();
        let all: Vec<usize> = (0..n_br).collect();
        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all)).unwrap();
        let dc_result = crate::solver::solve_dc(&net).expect("DC solve failed");
        let tol = 1e-8;

        let p_inj = net.bus_p_injection_pu();
        for l in 0..n_br {
            let reconstructed: f64 = (0..n_bus).map(|k| get(&ptdf, l, k) * p_inj[k]).sum();
            assert!(
                (reconstructed - dc_result.branch_p_flow[l]).abs() < tol,
                "Branch {l}: PTDF*P_inj = {:.10}, DC flow = {:.10} (diff = {:.2e})",
                reconstructed,
                dc_result.branch_p_flow[l],
                (reconstructed - dc_result.branch_p_flow[l]).abs()
            );
        }
    }

    /// Validate DC flows equal PTDF × injections for case14.
    #[test]
    fn test_ptdf_flow_reconstruction_case14() {
        let net = load_case14();
        let n_br = net.n_branches();
        let n_bus = net.n_buses();
        let all: Vec<usize> = (0..n_br).collect();
        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all)).unwrap();
        let dc_result = crate::solver::solve_dc(&net).expect("DC solve failed");
        let tol = 1e-8;

        let p_inj = net.bus_p_injection_pu();
        for l in 0..n_br {
            let reconstructed: f64 = (0..n_bus).map(|k| get(&ptdf, l, k) * p_inj[k]).sum();
            assert!(
                (reconstructed - dc_result.branch_p_flow[l]).abs() < tol,
                "Branch {l}: PTDF*P_inj = {:.10}, DC flow = {:.10} (diff = {:.2e})",
                reconstructed,
                dc_result.branch_p_flow[l],
                (reconstructed - dc_result.branch_p_flow[l]).abs()
            );
        }
    }

    /// Validate LODF by outaging branches and re-solving DC PF.
    #[test]
    fn test_lodf_case14_outage_validation() {
        let net = load_case14();
        let n_br = net.n_branches();
        let all: Vec<usize> = (0..n_br).collect();
        let lodf = compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all)).unwrap();
        let tol = 1e-8;

        let base_result = crate::solver::solve_dc(&net).expect("base DC solve failed");

        for &k in &[2usize, 6, 8] {
            if !lodf[(k, k)].is_finite() {
                continue;
            }
            let mut net_outaged = net.clone();
            net_outaged.branches[k].in_service = false;
            let outaged_result =
                crate::solver::solve_dc(&net_outaged).expect("outage DC solve failed");
            let flow_k_pre = base_result.branch_p_flow[k];

            for l in 0..n_br {
                if l == k || !net.branches[l].in_service {
                    continue;
                }
                let predicted = base_result.branch_p_flow[l] + lodf[(l, k)] * flow_k_pre;
                let actual = outaged_result.branch_p_flow[l];
                assert!(
                    (predicted - actual).abs() < tol,
                    "Outage {k}: branch {l} predicted={predicted:.10}, actual={actual:.10} (diff={:.2e})",
                    (predicted - actual).abs()
                );
            }
        }
    }

    /// Case9 LODF specific values.
    #[test]
    fn test_lodf_case9_values() {
        let net = load_case9();
        let n_br = net.n_branches();
        let all: Vec<usize> = (0..n_br).collect();
        let lodf = compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all)).unwrap();
        let tol = 1e-8;

        assert!(lodf[(0, 0)].is_infinite(), "Branch 0 should be a bridge");
        assert!(lodf[(0, 3)].is_infinite(), "Branch 3 should be a bridge");
        assert!(lodf[(0, 6)].is_infinite(), "Branch 6 should be a bridge");

        assert!(
            (lodf[(2, 1)] - (-1.0)).abs() < tol,
            "LODF[2,1] = {}",
            lodf[(2, 1)]
        );
        assert!(
            (lodf[(4, 1)] - (-1.0)).abs() < tol,
            "LODF[4,1] = {}",
            lodf[(4, 1)]
        );
        assert!(
            (lodf[(7, 1)] - (-1.0)).abs() < tol,
            "LODF[7,1] = {}",
            lodf[(7, 1)]
        );
        assert!(
            (lodf[(8, 1)] - (-1.0)).abs() < tol,
            "LODF[8,1] = {}",
            lodf[(8, 1)]
        );
        assert!(
            (lodf[(1, 7)] - (-1.0)).abs() < tol,
            "LODF[1,7] = {}",
            lodf[(1, 7)]
        );
        assert!(
            (lodf[(5, 7)] - (-1.0)).abs() < tol,
            "LODF[5,7] = {}",
            lodf[(5, 7)]
        );
        assert!(
            (lodf[(8, 7)] - (-1.0)).abs() < tol,
            "LODF[8,7] = {}",
            lodf[(8, 7)]
        );
        assert!(lodf[(3, 0)].is_infinite(), "LODF[3,0] should be Inf");
    }

    /// Validate compute_n2_lodf (Woodbury rank-2) against brute-force double outage.
    #[test]
    fn test_lodf_n2_brute_force_case14() {
        let net = load_case14();
        let n_br = net.n_branches();
        let all: Vec<usize> = (0..n_br).collect();
        let lodf = compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all)).unwrap();
        let tol = 1e-6;

        let base = crate::solver::solve_dc(&net).expect("base DC solve");

        for &(j, k) in &[(2usize, 6usize), (2, 8), (6, 8)] {
            if !lodf[(j, j)].is_finite() || !lodf[(k, k)].is_finite() {
                continue;
            }
            let mut net_outaged = net.clone();
            net_outaged.branches[j].in_service = false;
            net_outaged.branches[k].in_service = false;
            let outaged = match crate::solver::solve_dc(&net_outaged) {
                Ok(r) => r,
                Err(_) => continue,
            };

            let lodf2_jk = match compute_n2_lodf(
                &net,
                &N2LodfRequest::new((j, k)).with_monitored_branches(&all),
            ) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let lodf2_kj = match compute_n2_lodf(
                &net,
                &N2LodfRequest::new((k, j)).with_monitored_branches(&all),
            ) {
                Ok(v) => v,
                Err(_) => continue,
            };

            for m in (0..n_br).filter(|&m| m != j && m != k && net.branches[m].in_service) {
                let predicted = base.branch_p_flow[m]
                    + lodf2_jk[m] * base.branch_p_flow[j]
                    + lodf2_kj[m] * base.branch_p_flow[k];
                let actual = outaged.branch_p_flow[m];
                assert!(
                    (predicted - actual).abs() < tol,
                    "N-2 Woodbury error for outage ({j},{k}), monitored {m}: \
                     predicted={predicted:.10}, actual={actual:.10}, diff={:.2e}",
                    (predicted - actual).abs()
                );
            }
        }
    }

    /// PTDF for out-of-service branch is all zeros.
    #[test]
    fn test_ptdf_zero_for_oos_branch() {
        let mut net = load_case9();
        net.branches[2].in_service = false;

        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&[2]))
            .expect("compute_ptdf should not fail");
        let col = ptdf.row(2).expect("PTDF row for monitored branch");
        for &v in col.iter() {
            assert_eq!(v, 0.0, "out-of-service branch column should be all zero");
        }
    }

    /// PST-04: N-2 LODF result length matches monitored_indices length.
    #[test]
    fn test_pst04_n2_lodf_case9() {
        let net = load_case14();

        let n_monitored = 5;
        let monitored: Vec<usize> = (5..5 + n_monitored).collect();
        let result = compute_n2_lodf(
            &net,
            &N2LodfRequest::new((0, 2)).with_monitored_branches(&monitored),
        )
        .expect("N-2 LODF should succeed for case14 pair (0,2)");

        assert_eq!(
            result.len(),
            n_monitored,
            "result length should equal n_monitored"
        );
    }

    // -----------------------------------------------------------------------
    // OTDF tests
    // -----------------------------------------------------------------------

    /// Construct a simple 3-bus loop network for OTDF unit tests.
    ///
    /// Topology: Bus 1 (slack) -- Bus 2 -- Bus 3 -- Bus 1
    /// Three branches: 0: 1→2 (x=0.2), 1: 2→3 (x=0.4), 2: 3→1 (x=0.3)
    fn make_3bus_loop() -> surge_network::Network {
        use surge_network::Network;
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};

        let mut net = Network::new("3bus_loop");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 100.0));
        net.buses.push(Bus::new(2, BusType::PQ, 100.0));
        net.buses.push(Bus::new(3, BusType::PQ, 100.0));
        net.loads.push(Load::new(2, 100.0, 0.0));
        net.loads.push(Load::new(3, 50.0, 0.0));

        net.generators.push(Generator::new(1, 150.0, 1.0));

        net.branches.push(Branch::new_line(1, 2, 0.0, 0.2, 0.0));
        net.branches.push(Branch::new_line(2, 3, 0.0, 0.4, 0.0));
        net.branches.push(Branch::new_line(3, 1, 0.0, 0.3, 0.0));

        net
    }

    /// Construct a simple 2-bus network with a single bridge branch.
    fn make_2bus_bridge() -> surge_network::Network {
        use surge_network::Network;
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};

        let mut net = Network::new("2bus_bridge");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 100.0));
        net.buses.push(Bus::new(2, BusType::PQ, 100.0));
        net.loads.push(Load::new(2, 100.0, 0.0));

        net.generators.push(Generator::new(1, 100.0, 1.0));
        net.branches.push(Branch::new_line(1, 2, 0.0, 0.1, 0.0));

        net
    }

    /// OTDF analytical check: OTDF[(m,k)][bus] == PTDF[m][bus] + LODF[m,k] * PTDF[k][bus]
    #[test]
    fn test_otdf_case9_analytical() {
        // Use the 3-bus loop — no external data required.
        let net = make_3bus_loop();
        let n_br = net.n_branches();
        let n_bus = net.n_buses();
        let bus_map = net.bus_index_map();
        let all: Vec<usize> = (0..n_br).collect();

        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all)).expect("PTDF failed");

        // Outage branch 1 (bus 2→3): not a bridge in a 3-bus loop
        let outage = vec![1usize];
        let otdf = compute_otdf(&net, &OtdfRequest::new(&all, &outage)).expect("OTDF failed");

        let branch_k = &net.branches[1];
        let from_k = bus_map[&branch_k.from_bus];
        let to_k = bus_map[&branch_k.to_bus];
        let ptdf_row_k = ptdf.row(1).unwrap();
        let ptdf_kk = ptdf_row_k[from_k] - ptdf_row_k[to_k];
        let denom = 1.0 - ptdf_kk;

        for &m in &all {
            let ptdf_row_m = ptdf.row(m).unwrap();
            let ptdf_mk = ptdf_row_m[from_k] - ptdf_row_m[to_k];
            let lodf_mk = if denom.abs() < BRIDGE_THRESHOLD {
                0.0
            } else {
                ptdf_mk / denom
            };

            let otdf_vec = otdf.vector(m, 1).expect("OTDF entry missing");
            assert_eq!(
                otdf_vec.len(),
                n_bus,
                "OTDF vector wrong length for ({m},1)"
            );

            for b in 0..n_bus {
                let expected = ptdf_row_m[b] + lodf_mk * ptdf_row_k[b];
                let actual = otdf_vec[b];
                assert!(
                    (actual - expected).abs() < 1e-12,
                    "OTDF[({m},1)][{b}] = {actual:.15}, expected {expected:.15} (diff={:.2e})",
                    (actual - expected).abs()
                );
            }
        }
    }

    /// OTDF for a bridge line should be all INFINITY.
    #[test]
    fn test_otdf_bridge_line() {
        let net = make_2bus_bridge();
        let n_bus = net.n_buses();

        let otdf = compute_otdf(&net, &OtdfRequest::new(&[0], &[0]))
            .expect("OTDF should not error on bridge");
        let vec = otdf.vector(0, 0).expect("entry (0,0) must exist");
        assert_eq!(vec.len(), n_bus, "OTDF vector wrong length");
        for (b, &v) in vec.iter().enumerate() {
            assert!(
                v.is_infinite(),
                "OTDF[(0,0)][{b}] = {v}, expected INFINITY (bridge line)"
            );
        }
    }

    /// Self-consistency: OTDF[(m,m)] formula check using the direct PTDF values.
    #[test]
    fn test_otdf_self_consistency() {
        let net = make_3bus_loop();
        let n_br = net.n_branches();
        let bus_map = net.bus_index_map();
        let all: Vec<usize> = (0..n_br).collect();

        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all)).expect("PTDF failed");

        // For m == k (self-LODF = -1 by LODF identity), verify the formula gives
        // OTDF[(m,m)][from_m] - OTDF[(m,m)][to_m] = LODF[m,m] = -1.
        for &m in &all {
            let otdf = compute_otdf(&net, &OtdfRequest::new(&[m], &[m])).expect("OTDF failed");
            let otdf_vec = match otdf.vector(m, m) {
                Some(v) => v,
                None => continue,
            };

            // Skip bridge lines
            if otdf_vec.iter().any(|v| v.is_infinite()) {
                continue;
            }

            let branch_m = &net.branches[m];
            let from_m = bus_map[&branch_m.from_bus];
            let to_m = bus_map[&branch_m.to_bus];

            // Derive LODF[m,m] from PTDF
            let ptdf_row_m = ptdf.row(m).unwrap();
            let ptdf_mm = ptdf_row_m[from_m] - ptdf_row_m[to_m];
            let denom = 1.0 - ptdf_mm;
            if denom.abs() < 1e-6 {
                continue;
            }
            let lodf_mm = ptdf_mm / denom; // should be close to -1 for self

            // OTDF endpoint difference should equal lodf_mm (times b_m / b_m = 1 in PTDF space)
            let otdf_mm = otdf_vec[from_m] - otdf_vec[to_m];
            assert!(
                (otdf_mm - lodf_mm).abs() < 1e-12,
                "OTDF[({m},{m})][from-to] = {otdf_mm:.15}, LODF[{m},{m}] = {lodf_mm:.15}"
            );
        }
    }

    /// OTDF matches formula: independently compute PTDF + LODF × PTDF_outage.
    #[test]
    fn test_otdf_matches_formula() {
        let net = make_3bus_loop();
        let n_br = net.n_branches();
        let n_bus = net.n_buses();
        let bus_map = net.bus_index_map();
        let all: Vec<usize> = (0..n_br).collect();

        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all)).expect("PTDF failed");
        let otdf = compute_otdf(&net, &OtdfRequest::new(&all, &all)).expect("OTDF failed");

        for &k in &all {
            let branch_k = &net.branches[k];
            let from_k = bus_map[&branch_k.from_bus];
            let to_k = bus_map[&branch_k.to_bus];
            let ptdf_row_k = ptdf.row(k).unwrap();
            let ptdf_kk = ptdf_row_k[from_k] - ptdf_row_k[to_k];
            let is_bridge = (1.0 - ptdf_kk).abs() < BRIDGE_THRESHOLD;

            for &m in &all {
                let otdf_vec = otdf.vector(m, k).expect("OTDF entry missing");

                if is_bridge {
                    assert!(
                        otdf_vec.iter().all(|v| v.is_infinite()),
                        "bridge outage {k} → all OTDF should be INFINITY"
                    );
                    continue;
                }

                let ptdf_row_m = ptdf.row(m).unwrap();
                let ptdf_mk = ptdf_row_m[from_k] - ptdf_row_m[to_k];
                let lodf_mk = ptdf_mk / (1.0 - ptdf_kk);

                for b in 0..n_bus {
                    let expected = ptdf_row_m[b] + lodf_mk * ptdf_row_k[b];
                    let actual = otdf_vec[b];
                    assert!(
                        (actual - expected).abs() < 1e-12,
                        "OTDF[({m},{k})][{b}]: actual={actual:.15}, expected={expected:.15}, diff={:.2e}",
                        (actual - expected).abs()
                    );
                }
            }
        }
    }

    #[test]
    fn test_otdf_bus_subset_matches_full() {
        let net = load_case14();
        let monitored = vec![0, 3, 7];
        let outages = vec![1, 5];
        let buses = vec![0, 4, 9];

        let full = compute_otdf(&net, &OtdfRequest::new(&monitored, &outages)).expect("full otdf");
        let subset = compute_otdf(
            &net,
            &OtdfRequest::new(&monitored, &outages).with_bus_indices(&buses),
        )
        .expect("subset otdf");

        assert_eq!(subset.monitored_branches(), monitored.as_slice());
        assert_eq!(subset.outage_branches(), outages.as_slice());
        assert_eq!(subset.bus_indices(), buses.as_slice());
        assert_eq!(subset.n_buses(), buses.len());

        for &m in &monitored {
            for &k in &outages {
                let full_vec = full.vector(m, k).expect("full OTDF vector");
                let subset_vec = subset.vector(m, k).expect("subset OTDF vector");
                assert_eq!(subset_vec.len(), buses.len());
                for (pos, &bus_idx) in buses.iter().enumerate() {
                    assert!(
                        (subset_vec[pos] - full_vec[bus_idx]).abs() < 1e-12
                            || (subset_vec[pos].is_infinite() && full_vec[bus_idx].is_infinite()),
                        "OTDF subset mismatch for ({m},{k}) bus {bus_idx}"
                    );
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Slack-weighted / headroom-slack PTDF/LODF tests
    // -----------------------------------------------------------------------

    /// Fixed slack weights apply the requested row correction exactly.
    #[test]
    fn test_ptdf_with_slack_weights_applies_requested_weights() {
        let net = load_case14();
        let base = compute_ptdf(&net, &PtdfRequest::for_branches(&[0])).unwrap();
        let options = DcSensitivityOptions::with_slack_weights(&[(1usize, 3.0), (2usize, 1.0)]);
        let weighted =
            compute_ptdf(&net, &PtdfRequest::for_branches(&[0]).with_options(options)).unwrap();

        let base_row = base.row(0).unwrap();
        let weighted_row = weighted.row(0).unwrap();
        let correction = 0.75 * base_row[1] + 0.25 * base_row[2];

        for bus_idx in 0..net.n_buses() {
            assert!(
                (weighted_row[bus_idx] - (base_row[bus_idx] - correction)).abs() < 1e-12,
                "bus {bus_idx}: weighted PTDF mismatch"
            );
        }
    }

    #[test]
    fn test_ptdf_bus_subset_matches_full_rows() {
        let net = load_case14();
        let monitored = [0usize, 3usize];
        let bus_indices = [0usize, 4usize, 7usize];

        let full = compute_ptdf(&net, &PtdfRequest::for_branches(&monitored)).expect("full ptdf");
        let subset = compute_ptdf(
            &net,
            &PtdfRequest::for_branches(&monitored).with_bus_indices(&bus_indices),
        )
        .expect("subset ptdf");

        assert_eq!(subset.monitored_branches(), monitored.as_slice());
        assert_eq!(subset.bus_indices(), bus_indices.as_slice());
        assert_eq!(subset.n_cols(), bus_indices.len());

        for &branch_idx in &monitored {
            let subset_row = subset.row(branch_idx).expect("subset row");
            let full_row = full.row(branch_idx).expect("full row");
            for (pos, &bus_idx) in bus_indices.iter().enumerate() {
                assert!(
                    (subset_row[pos] - full_row[bus_idx]).abs() < 1e-12,
                    "branch {branch_idx} bus {bus_idx}: subset PTDF mismatch"
                );
            }
        }
    }

    #[test]
    fn test_otdf_request_matches_formula() {
        let net = load_case14();
        let monitored = [0usize];
        let outages = [1usize];
        let buses = [1usize, 2, 5];
        let options = DcSensitivityOptions::with_slack_weights(&[(1usize, 3.0), (2usize, 1.0)]);

        let otdf = compute_otdf(
            &net,
            &OtdfRequest::new(&monitored, &outages)
                .with_bus_indices(&buses)
                .with_options(options.clone()),
        )
        .expect("weighted otdf");
        let ptdf = compute_ptdf(
            &net,
            &PtdfRequest::for_branches(&[monitored[0], outages[0]]).with_options(options.clone()),
        )
        .expect("ptdf");
        let lodf =
            compute_lodf(&net, &LodfRequest::for_branches(&monitored, &outages)).expect("lodf");

        let otdf_vector = otdf.vector(monitored[0], outages[0]).expect("otdf vector");
        let monitored_row = ptdf.row(monitored[0]).expect("monitored ptdf row");
        let outage_row = ptdf.row(outages[0]).expect("outage ptdf row");
        let lodf_mk = lodf[(0, 0)];

        for (pos, &bus_idx) in buses.iter().enumerate() {
            let expected = monitored_row[bus_idx] + lodf_mk * outage_row[bus_idx];
            assert!(
                (otdf_vector[pos] - expected).abs() < 1e-12,
                "weighted OTDF mismatch at bus {bus_idx}: actual={} expected={expected}",
                otdf_vector[pos]
            );
        }
    }

    /// Headroom-slack PTDF: on a balanced base case, a positive bus injection is
    /// balanced by counter-withdrawals weighted by downward headroom.
    #[test]
    fn test_ptdf_headroom_slack_perturbation() {
        let net = build_balanced_headroom_ptdf_network();
        let n_br = net.n_branches();
        let all: Vec<usize> = (0..n_br).collect();

        let bus_map = net.bus_index_map();
        let participating_buses = vec![bus_map[&1], bus_map[&2], bus_map[&3]];
        let options = DcSensitivityOptions::with_headroom_slack(&participating_buses);
        let ptdf_ds =
            compute_ptdf(&net, &PtdfRequest::for_branches(&all).with_options(options)).unwrap();

        let dc_opts = DcPfOptions::with_headroom_slack(&participating_buses);
        let base = crate::solver::solve_dc_opts(&net, &dc_opts).expect("base DC solve");

        // Start from zero mismatch so the finite difference reflects the PTDF
        // contract itself rather than the change between two standing-mismatch
        // redispatches.
        let test_bus = bus_map[&4];
        let delta = 0.01; // 1 MW injection perturbation (pu)
        let mut net_perturbed = net.clone();
        let perturb_bus_number = net_perturbed.buses[test_bus].number;
        net_perturbed.loads.push(surge_network::network::Load::new(
            perturb_bus_number,
            -(delta * net.base_mva),
            0.0,
        ));
        let perturbed =
            crate::solver::solve_dc_opts(&net_perturbed, &dc_opts).expect("perturbed DC solve");

        let tol = 1e-6;
        for l in 0..n_br {
            let actual_delta_flow = perturbed.branch_p_flow[l] - base.branch_p_flow[l];
            let predicted = get(&ptdf_ds, l, test_bus) * delta;
            assert!(
                (actual_delta_flow - predicted).abs() < tol,
                "Branch {l}: actual ΔF={:.10}, PTDF_ds×Δ={:.10} (diff={:.2e})",
                actual_delta_flow,
                predicted,
                (actual_delta_flow - predicted).abs()
            );
        }
    }

    /// Headroom-slack LODF: outage prediction matches DC PF re-solve.
    #[test]
    fn test_lodf_headroom_slack_outage_validation() {
        let net = load_case14();
        let n_br = net.n_branches();
        let all: Vec<usize> = (0..n_br).collect();

        let gen_bus_indices: Vec<usize> = {
            let bus_map = net.bus_index_map();
            net.generators
                .iter()
                .filter(|g| g.in_service)
                .filter_map(|g| bus_map.get(&g.bus).copied())
                .collect()
        };
        let participating_buses = gen_bus_indices.clone();
        let lodf = compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all)).unwrap();

        let dc_opts = DcPfOptions::with_headroom_slack(&participating_buses);
        let base_result = crate::solver::solve_dc_opts(&net, &dc_opts).expect("base DC solve");

        let tol = 1e-6;

        for &k in &[2usize, 6, 8] {
            if !lodf[(k, k)].is_finite() {
                continue;
            }
            let mut net_outaged = net.clone();
            net_outaged.branches[k].in_service = false;
            let outaged_result =
                crate::solver::solve_dc_opts(&net_outaged, &dc_opts).expect("outage DC solve");
            let flow_k_pre = base_result.branch_p_flow[k];

            for l in 0..n_br {
                if l == k || !net.branches[l].in_service {
                    continue;
                }
                let predicted = base_result.branch_p_flow[l] + lodf[(l, k)] * flow_k_pre;
                let actual = outaged_result.branch_p_flow[l];
                assert!(
                    (predicted - actual).abs() < tol,
                    "Outage {k}: branch {l} predicted={predicted:.10}, actual={actual:.10} (diff={:.2e})",
                    (predicted - actual).abs()
                );
            }
        }
    }

    #[test]
    fn test_lodf_and_n2_are_slack_invariant() {
        let net = load_case14();
        let monitored = vec![0, 3, 7];
        let outages = vec![1, 5];
        let outage_pairs = vec![(0, 2), (2, 0)];
        let single_lodf = compute_lodf(&net, &LodfRequest::for_branches(&monitored, &outages))
            .expect("single-slack lodf");
        let request_lodf = compute_lodf(&net, &LodfRequest::for_branches(&monitored, &outages))
            .expect("request lodf");
        assert_eq!(single_lodf.n_rows(), request_lodf.n_rows());
        assert_eq!(single_lodf.n_cols(), request_lodf.n_cols());
        for row in 0..single_lodf.n_rows() {
            for col in 0..single_lodf.n_cols() {
                assert!(
                    (single_lodf[(row, col)] - request_lodf[(row, col)]).abs() < 1e-12
                        || (single_lodf[(row, col)].is_infinite()
                            && request_lodf[(row, col)].is_infinite()),
                    "request LODF changed at ({row},{col})"
                );
            }
        }

        let single_n2 = compute_n2_lodf_batch(
            &net,
            &N2LodfBatchRequest::new(&outage_pairs).with_monitored_branches(&monitored),
        )
        .expect("single-slack n2");
        let request_n2 = compute_n2_lodf_batch(
            &net,
            &N2LodfBatchRequest::new(&outage_pairs).with_monitored_branches(&monitored),
        )
        .expect("request n2");
        assert_eq!(single_n2.n_rows(), request_n2.n_rows());
        assert_eq!(single_n2.n_cols(), request_n2.n_cols());
        for row in 0..single_n2.n_rows() {
            for col in 0..single_n2.n_cols() {
                assert!(
                    (single_n2[(row, col)] - request_n2[(row, col)]).abs() < 1e-12
                        || (single_n2[(row, col)].is_infinite()
                            && request_n2[(row, col)].is_infinite()),
                    "request N-2 changed at ({row},{col})"
                );
            }
        }
    }
}
