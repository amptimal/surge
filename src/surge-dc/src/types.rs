// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Data types for DC power flow results, requests, and errors.

use std::collections::HashMap;
use std::ops::Index;

use faer::Mat;
use serde::{Deserialize, Serialize};
use surge_network::AngleReference;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Minimum absolute reactance for a branch to participate in DC power flow.
///
/// Branches with `|x| < MIN_REACTANCE` are treated as zero-impedance ties and
/// excluded from the B' matrix, sensitivity computations, and flow calculations.
/// This matches the threshold used by [`Branch::b_dc()`](surge_network::network::Branch::b_dc).
pub(crate) const MIN_REACTANCE: f64 = 1e-20;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors arising from DC power flow or sensitivity computation.
#[derive(Debug, thiserror::Error)]
pub enum DcError {
    /// The network contains no buses.
    #[error("network has no buses")]
    EmptyNetwork,

    /// No slack (reference) bus found in the network or island.
    #[error("network has no slack bus")]
    NoSlackBus,

    /// The B' susceptance matrix is singular, typically due to a disconnected network.
    #[error("singular B' matrix -- network may be disconnected")]
    SingularMatrix,

    /// The requested computation is infeasible (e.g., bridge-line N-2 outage).
    #[error("N-2 LODF computation error: {0}")]
    Infeasible(String),

    /// The network model is invalid (e.g., out-of-range bus or branch index).
    #[error("invalid network: {0}")]
    InvalidNetwork(String),
}

// ---------------------------------------------------------------------------
// Power flow solution and options
// ---------------------------------------------------------------------------

/// Result of a DC power flow computation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DcPfSolution {
    /// Bus voltage angles in radians (indexed by internal bus order).
    pub theta: Vec<f64>,
    /// Real power flow on each branch in per-unit (positive = from->to).
    pub branch_p_flow: Vec<f64>,
    /// Slack bus real power injection in per-unit.
    pub slack_p_injection: f64,
    /// Solve time in seconds.
    pub solve_time_secs: f64,
    /// Total system generation in MW (only populated for headroom-slack solves).
    #[serde(default)]
    pub total_generation_mw: f64,
    /// Per-bus slack absorption: internal bus index -> MW absorbed.
    /// Non-empty only when headroom slack is used.
    #[serde(default)]
    pub slack_distribution: HashMap<usize, f64>,
    /// Effective real power injection at each bus in per-unit (full bus order).
    ///
    /// For non-slack buses this is the actual RHS used in the solve: scheduled
    /// Pg − Pd plus Gs shunt losses, PST phantom injections, two-terminal HVDC
    /// schedules, MTDC converter injections, and any distributed-slack shares.
    /// For the slack bus it is the back-calculated KCL value from branch flows.
    ///
    /// Use this field — not `bus_p_injection_pu()` — when reporting or
    /// post-processing injection balance, because `bus_p_injection_pu()` omits
    /// the correction terms above and will not balance against branch flows once
    /// those features are active.
    pub p_inject_pu: Vec<f64>,
    /// Island assignment for each bus (indexed by internal bus index).
    /// `island_ids[i]` is the island index for bus `i`.
    #[serde(default)]
    pub island_ids: Vec<usize>,
}

/// Options controlling the DC power flow solve.
///
/// All fields are optional -- pass `DcPfOptions::default()` for the classic
/// single-slack behavior.
#[derive(Debug, Clone, Default)]
pub struct DcPfOptions {
    /// Buses participating in headroom-limited slack balancing.
    ///
    /// When `Some`, the total power imbalance is redistributed among the listed
    /// buses in proportion to available generator headroom at those buses.
    ///
    /// When `None` (default), the classic single-bus slack formulation is used.
    pub headroom_slack_bus_indices: Option<Vec<usize>>,

    /// Participation-factor slack weights: `(internal_bus_index, weight)`.
    ///
    /// When `Some`, the total power imbalance is redistributed among the
    /// listed buses in proportion to their weights (normalized internally to
    /// sum to 1.0). This is the RTO-style distributed slack model where
    /// weights typically come from generator AGC participation factors
    /// aggregated by bus.
    ///
    /// Takes precedence over `headroom_slack_bus_indices` when both are set.
    pub participation_factors: Option<Vec<(usize, f64)>>,

    /// Output angle reference convention for reported `theta`.
    ///
    /// This affects only the returned bus angles. It does not affect branch
    /// flows, slack redistribution, or the DC power balance itself.
    pub angle_reference: AngleReference,

    /// External real-power injections added to the DC power flow RHS.
    ///
    /// Each entry is `(bus_number, p_mw)` where `bus_number` is the external
    /// bus number (matching `Bus::number`) and `p_mw` is the injection in MW
    /// (positive = generation / injection into AC network, negative = load /
    /// withdrawal from AC network).
    ///
    /// Use this to pass pre-computed MTDC converter injections, external
    /// schedules, or any other bus-level P corrections that should be folded
    /// into the DC power flow injection vector.
    pub external_p_injections_mw: Vec<(u32, f64)>,
}

impl DcPfOptions {
    /// Build options with headroom-limited slack balancing on the listed buses.
    ///
    /// Indices are *internal* (0-based array position in `Network::buses`),
    /// not external bus numbers.
    pub fn with_headroom_slack(participating_bus_indices: &[usize]) -> Self {
        if participating_bus_indices.is_empty() {
            return Self::default();
        }
        DcPfOptions {
            headroom_slack_bus_indices: Some(participating_bus_indices.to_vec()),
            ..Self::default()
        }
    }

    /// Build options with explicit participation-factor slack weights.
    ///
    /// Each entry is `(internal_bus_index, weight)`. Weights are normalized
    /// internally. Zero and negative weights are ignored.
    pub fn with_participation_factors(weights: &[(usize, f64)]) -> Self {
        let filtered: Vec<(usize, f64)> = weights
            .iter()
            .copied()
            .filter(|&(_, w)| w > 0.0 && w.is_finite())
            .collect();
        if filtered.is_empty() {
            return Self::default();
        }
        DcPfOptions {
            participation_factors: Some(filtered),
            ..Self::default()
        }
    }

    /// Build options using AGC participation factors from the network's generators.
    ///
    /// Aggregates `Generator::agc_participation_factor` by bus, producing
    /// the standard RTO-style distributed slack model. Falls back to
    /// single-slack if no generators have participation factors set.
    pub fn with_network_participation(network: &surge_network::Network) -> Self {
        let weights = network.agc_participation_by_bus();
        if weights.is_empty() {
            return Self::default();
        }
        DcPfOptions {
            participation_factors: Some(weights),
            ..Self::default()
        }
    }

    /// Set the output angle reference convention.
    pub fn with_angle_reference(mut self, angle_reference: AngleReference) -> Self {
        self.angle_reference = angle_reference;
        self
    }
}

// ---------------------------------------------------------------------------
// Analysis request / result
// ---------------------------------------------------------------------------

/// Canonical request for a one-pass DC sensitivity workflow.
///
/// This computes DC power flow plus PTDF for a monitored branch set, and
/// optionally includes OTDF, subset LODF, and batched N-2 sensitivities over
/// the same monitored set.
#[derive(Debug, Clone, Default)]
pub struct DcAnalysisRequest {
    /// Branch indices to monitor for PTDF / LODF / N-2 results.
    ///
    /// When `None`, all branches in `network.branches` are monitored.
    pub monitored_branch_indices: Option<Vec<usize>>,
    /// Optional bus subset for PTDF output vectors.
    ///
    /// When `None`, PTDF is returned for all buses. When `Some`, PTDF vectors
    /// are returned only for the requested internal bus indices in the given order.
    pub ptdf_bus_indices: Option<Vec<usize>>,
    /// Explicit outage branches for subset LODF.
    ///
    /// When empty, no LODF subset is computed.
    pub lodf_outage_branch_indices: Vec<usize>,
    /// Explicit outage branches for subset OTDF.
    ///
    /// When empty, no OTDF subset is computed.
    pub otdf_outage_branch_indices: Vec<usize>,
    /// Optional bus subset for OTDF output vectors.
    ///
    /// When `None`, OTDF is returned for all buses. When `Some`, OTDF vectors
    /// are returned only for the requested internal bus indices in the given order.
    pub otdf_bus_indices: Option<Vec<usize>>,
    /// Explicit outage pairs for batched N-2 sensitivity computation.
    ///
    /// When empty, no N-2 batch is computed.
    pub n2_outage_pairs: Vec<(usize, usize)>,
    /// DC power flow options applied to the base solve.
    pub pf_options: DcPfOptions,
    /// Optional sensitivity options applied to PTDF / OTDF outputs.
    ///
    /// When `None`, the sensitivity workflow derives its slack policy from
    /// `pf_options`: headroom-slack solves use headroom-slack sensitivities,
    /// otherwise the classic single-slack sensitivity formulation is used.
    pub sensitivity_options: Option<crate::sensitivity::DcSensitivityOptions>,
}

impl DcAnalysisRequest {
    /// Monitor all branches and compute only the base DC power flow + PTDF.
    pub fn all_branches() -> Self {
        Self::default()
    }

    /// Monitor the requested branch set.
    pub fn with_monitored_branches(monitored_branch_indices: &[usize]) -> Self {
        Self {
            monitored_branch_indices: Some(monitored_branch_indices.to_vec()),
            ..Self::default()
        }
    }

    /// Restrict PTDF output to the requested internal bus indices.
    pub fn with_ptdf_buses(mut self, bus_indices: &[usize]) -> Self {
        self.ptdf_bus_indices = Some(bus_indices.to_vec());
        self
    }

    /// Add a subset LODF request for the given outage branches.
    pub fn with_lodf_outages(mut self, outage_branch_indices: &[usize]) -> Self {
        self.lodf_outage_branch_indices = outage_branch_indices.to_vec();
        self
    }

    /// Add a subset OTDF request for the given outage branches.
    pub fn with_otdf_outages(mut self, outage_branch_indices: &[usize]) -> Self {
        self.otdf_outage_branch_indices = outage_branch_indices.to_vec();
        self
    }

    /// Restrict OTDF output to the requested internal bus indices.
    pub fn with_otdf_buses(mut self, bus_indices: &[usize]) -> Self {
        self.otdf_bus_indices = Some(bus_indices.to_vec());
        self
    }

    /// Add a batched N-2 request for the given outage pairs.
    pub fn with_n2_outage_pairs(mut self, outage_pairs: &[(usize, usize)]) -> Self {
        self.n2_outage_pairs = outage_pairs.to_vec();
        self
    }

    /// Apply DC power flow options to the base solve.
    pub fn with_pf_options(mut self, pf_options: DcPfOptions) -> Self {
        self.pf_options = pf_options;
        self
    }

    /// Apply explicit sensitivity options to PTDF / OTDF outputs.
    pub fn with_sensitivity_options(
        mut self,
        sensitivity_options: crate::sensitivity::DcSensitivityOptions,
    ) -> Self {
        self.sensitivity_options = Some(sensitivity_options);
        self
    }
}

/// Result of the canonical one-pass DC sensitivity workflow.
#[derive(Debug, Clone)]
pub struct DcAnalysisResult {
    /// Base DC power flow solution for the requested slack policy.
    pub power_flow: DcPfSolution,
    /// Monitored branch set used by PTDF / LODF / N-2 outputs.
    pub monitored_branch_indices: Vec<usize>,
    /// PTDF rows in monitored-branch order.
    pub ptdf: PtdfRows,
    /// Bus order for `ptdf`.
    pub ptdf_bus_indices: Vec<usize>,
    /// Optional monitored-by-outage-by-bus OTDF tensor.
    pub otdf: Option<OtdfResult>,
    /// Outage order for `otdf`, when present.
    pub otdf_outage_branch_indices: Vec<usize>,
    /// Bus order for `otdf`, when present.
    pub otdf_bus_indices: Vec<usize>,
    /// Optional rectangular monitored-by-outage LODF matrix.
    pub lodf: Option<LodfResult>,
    /// Column order for `lodf`, when present.
    pub lodf_outage_branch_indices: Vec<usize>,
    /// Optional monitored-by-outage-pair N-2 matrix.
    pub n2_lodf: Option<N2LodfBatchResult>,
    /// Column order for `n2_lodf`, when present.
    pub n2_outage_pairs: Vec<(usize, usize)>,
}

// ---------------------------------------------------------------------------
// PTDF
// ---------------------------------------------------------------------------

/// PTDF rows stored in monitored-branch order with dense row-major bus data.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PtdfRows {
    monitored_branch_indices: Vec<usize>,
    row_positions: Vec<Option<usize>>,
    bus_indices: Vec<usize>,
    bus_positions: Vec<Option<usize>>,
    n_values_per_row: usize,
    values: Vec<f64>,
}

impl PtdfRows {
    pub(crate) fn new(
        monitored_branch_indices: &[usize],
        bus_indices: &[usize],
        n_branches: usize,
        n_total_buses: usize,
    ) -> Self {
        let mut row_positions = vec![None; n_branches];
        for (row_pos, &branch_idx) in monitored_branch_indices.iter().enumerate() {
            if branch_idx < n_branches && row_positions[branch_idx].is_none() {
                row_positions[branch_idx] = Some(row_pos);
            }
        }

        let mut bus_positions = vec![None; n_total_buses];
        for (bus_pos, &bus_idx) in bus_indices.iter().enumerate() {
            if bus_idx < n_total_buses && bus_positions[bus_idx].is_none() {
                bus_positions[bus_idx] = Some(bus_pos);
            }
        }
        Self {
            monitored_branch_indices: monitored_branch_indices.to_vec(),
            row_positions,
            bus_indices: bus_indices.to_vec(),
            bus_positions,
            n_values_per_row: bus_indices.len(),
            values: vec![0.0; monitored_branch_indices.len() * bus_indices.len()],
        }
    }

    /// Returns the monitored branch indices in row order.
    #[inline]
    pub fn monitored_branches(&self) -> &[usize] {
        &self.monitored_branch_indices
    }

    /// Returns the bus indices in column order.
    #[inline]
    pub fn bus_indices(&self) -> &[usize] {
        &self.bus_indices
    }

    /// Number of monitored branches (rows).
    #[inline]
    pub fn n_rows(&self) -> usize {
        self.monitored_branch_indices.len()
    }

    /// Number of buses (columns) per PTDF row.
    #[inline]
    pub fn n_cols(&self) -> usize {
        self.n_values_per_row
    }

    /// Returns the PTDF row for `branch_idx`, or `None` if not monitored.
    #[inline]
    pub fn row(&self, branch_idx: usize) -> Option<&[f64]> {
        let row_pos = self.row_positions.get(branch_idx).copied().flatten()?;
        Some(self.row_at(row_pos))
    }

    /// Returns the PTDF row at positional index `row_pos`.
    #[inline]
    pub fn row_at(&self, row_pos: usize) -> &[f64] {
        let start = row_pos * self.n_values_per_row;
        &self.values[start..start + self.n_values_per_row]
    }

    #[inline(always)]
    pub(crate) fn row_at_mut(&mut self, row_pos: usize) -> &mut [f64] {
        let start = row_pos * self.n_values_per_row;
        &mut self.values[start..start + self.n_values_per_row]
    }

    /// Returns the PTDF value for `(branch_idx, bus_idx)`, or 0.0 if not present.
    #[inline]
    pub fn get(&self, branch_idx: usize, bus_idx: usize) -> f64 {
        let Some(bus_pos) = self.bus_positions.get(bus_idx).copied().flatten() else {
            return 0.0;
        };
        self.row(branch_idx)
            .and_then(|row| row.get(bus_pos))
            .copied()
            .unwrap_or(0.0)
    }

    /// Decompose into `(monitored_branch_indices, bus_indices, values)`.
    pub fn into_parts(self) -> (Vec<usize>, Vec<usize>, Vec<f64>) {
        (self.monitored_branch_indices, self.bus_indices, self.values)
    }
}

// ---------------------------------------------------------------------------
// OTDF
// ---------------------------------------------------------------------------

/// OTDF vectors stored in monitored-by-outage order with dense row-major bus data.
#[derive(Debug, Clone, PartialEq)]
pub struct OtdfResult {
    monitored_branch_indices: Vec<usize>,
    outage_branch_indices: Vec<usize>,
    bus_indices: Vec<usize>,
    monitored_positions: Vec<Option<usize>>,
    outage_positions: Vec<Option<usize>>,
    bus_positions: Vec<Option<usize>>,
    n_values_per_vector: usize,
    values: Vec<f64>,
}

impl OtdfResult {
    pub(crate) fn new(
        monitored_branch_indices: &[usize],
        outage_branch_indices: &[usize],
        bus_indices: &[usize],
        n_branches: usize,
        n_total_buses: usize,
    ) -> Self {
        let mut monitored_positions = vec![None; n_branches];
        for (row_pos, &branch_idx) in monitored_branch_indices.iter().enumerate() {
            if branch_idx < n_branches && monitored_positions[branch_idx].is_none() {
                monitored_positions[branch_idx] = Some(row_pos);
            }
        }

        let mut outage_positions = vec![None; n_branches];
        for (col_pos, &branch_idx) in outage_branch_indices.iter().enumerate() {
            if branch_idx < n_branches && outage_positions[branch_idx].is_none() {
                outage_positions[branch_idx] = Some(col_pos);
            }
        }

        let mut bus_positions = vec![None; n_total_buses];
        for (bus_pos, &bus_idx) in bus_indices.iter().enumerate() {
            if bus_idx < n_total_buses && bus_positions[bus_idx].is_none() {
                bus_positions[bus_idx] = Some(bus_pos);
            }
        }

        Self {
            monitored_branch_indices: monitored_branch_indices.to_vec(),
            outage_branch_indices: outage_branch_indices.to_vec(),
            bus_indices: bus_indices.to_vec(),
            monitored_positions,
            outage_positions,
            bus_positions,
            n_values_per_vector: bus_indices.len(),
            values: vec![
                0.0;
                monitored_branch_indices.len()
                    * outage_branch_indices.len()
                    * bus_indices.len()
            ],
        }
    }

    /// Returns the monitored branch indices in row order.
    #[inline]
    pub fn monitored_branches(&self) -> &[usize] {
        &self.monitored_branch_indices
    }

    /// Returns the outage branch indices in column order.
    #[inline]
    pub fn outage_branches(&self) -> &[usize] {
        &self.outage_branch_indices
    }

    /// Returns the bus indices for each OTDF vector.
    #[inline]
    pub fn bus_indices(&self) -> &[usize] {
        &self.bus_indices
    }

    /// Number of monitored branches.
    #[inline]
    pub fn n_monitored(&self) -> usize {
        self.monitored_branch_indices.len()
    }

    /// Number of outage branches.
    #[inline]
    pub fn n_outages(&self) -> usize {
        self.outage_branch_indices.len()
    }

    /// Number of buses per OTDF vector.
    #[inline]
    pub fn n_buses(&self) -> usize {
        self.n_values_per_vector
    }

    /// Returns the OTDF vector for a `(monitored, outage)` pair, or `None` if not present.
    #[inline]
    pub fn vector(&self, monitored_branch_idx: usize, outage_branch_idx: usize) -> Option<&[f64]> {
        let monitored_pos = self
            .monitored_positions
            .get(monitored_branch_idx)
            .copied()
            .flatten()?;
        let outage_pos = self
            .outage_positions
            .get(outage_branch_idx)
            .copied()
            .flatten()?;
        Some(self.vector_at(monitored_pos, outage_pos))
    }

    /// Returns the OTDF vector at positional indices `(monitored_pos, outage_pos)`.
    #[inline]
    pub fn vector_at(&self, monitored_pos: usize, outage_pos: usize) -> &[f64] {
        let start = (monitored_pos * self.n_outages() + outage_pos) * self.n_values_per_vector;
        &self.values[start..start + self.n_values_per_vector]
    }

    #[inline(always)]
    pub(crate) fn vector_at_mut(&mut self, monitored_pos: usize, outage_pos: usize) -> &mut [f64] {
        let start = (monitored_pos * self.n_outages() + outage_pos) * self.n_values_per_vector;
        &mut self.values[start..start + self.n_values_per_vector]
    }

    /// Returns the OTDF value for `(monitored, outage, bus)`, or 0.0 if not present.
    #[inline]
    pub fn get(
        &self,
        monitored_branch_idx: usize,
        outage_branch_idx: usize,
        bus_idx: usize,
    ) -> f64 {
        let Some(bus_pos) = self.bus_positions.get(bus_idx).copied().flatten() else {
            return 0.0;
        };
        self.vector(monitored_branch_idx, outage_branch_idx)
            .and_then(|vector| vector.get(bus_pos))
            .copied()
            .unwrap_or(0.0)
    }

    /// Decompose into `(monitored_indices, outage_indices, bus_indices, values)`.
    pub fn into_parts(self) -> (Vec<usize>, Vec<usize>, Vec<usize>, Vec<f64>) {
        (
            self.monitored_branch_indices,
            self.outage_branch_indices,
            self.bus_indices,
            self.values,
        )
    }
}

// ---------------------------------------------------------------------------
// LODF
// ---------------------------------------------------------------------------

/// Rectangular LODF result for explicit monitored and outage branch sets.
#[derive(Debug, Clone, PartialEq)]
pub struct LodfResult {
    monitored_branch_indices: Vec<usize>,
    outage_branch_indices: Vec<usize>,
    values: Mat<f64>,
}

impl LodfResult {
    pub(crate) fn new(
        monitored_branch_indices: &[usize],
        outage_branch_indices: &[usize],
        values: Mat<f64>,
    ) -> Self {
        Self {
            monitored_branch_indices: monitored_branch_indices.to_vec(),
            outage_branch_indices: outage_branch_indices.to_vec(),
            values,
        }
    }

    /// Returns the monitored branch indices (row order).
    #[inline]
    pub fn monitored_branches(&self) -> &[usize] {
        &self.monitored_branch_indices
    }

    /// Returns the outage branch indices (column order).
    #[inline]
    pub fn outage_branches(&self) -> &[usize] {
        &self.outage_branch_indices
    }

    /// Number of monitored branches (rows).
    #[inline]
    pub fn n_rows(&self) -> usize {
        self.values.nrows()
    }

    /// Number of outage branches (columns).
    #[inline]
    pub fn n_cols(&self) -> usize {
        self.values.ncols()
    }

    /// Returns a reference to the underlying dense LODF matrix.
    #[inline]
    pub fn matrix(&self) -> &Mat<f64> {
        &self.values
    }

    /// Decompose into `(monitored_indices, outage_indices, values)`.
    #[inline]
    pub fn into_parts(self) -> (Vec<usize>, Vec<usize>, Mat<f64>) {
        (
            self.monitored_branch_indices,
            self.outage_branch_indices,
            self.values,
        )
    }
}

impl Index<(usize, usize)> for LodfResult {
    type Output = f64;

    fn index(&self, index: (usize, usize)) -> &Self::Output {
        &self.values[index]
    }
}

/// Dense all-pairs LODF result for one branch set.
#[derive(Debug, Clone, PartialEq)]
pub struct LodfMatrixResult {
    branch_indices: Vec<usize>,
    values: Mat<f64>,
}

impl LodfMatrixResult {
    pub(crate) fn new(branch_indices: &[usize], values: Mat<f64>) -> Self {
        Self {
            branch_indices: branch_indices.to_vec(),
            values,
        }
    }

    /// Returns the branch indices used as both rows and columns.
    #[inline]
    pub fn branches(&self) -> &[usize] {
        &self.branch_indices
    }

    /// Number of rows (branches).
    #[inline]
    pub fn n_rows(&self) -> usize {
        self.values.nrows()
    }

    /// Number of columns (branches).
    #[inline]
    pub fn n_cols(&self) -> usize {
        self.values.ncols()
    }

    /// Returns a reference to the underlying dense all-pairs LODF matrix.
    #[inline]
    pub fn matrix(&self) -> &Mat<f64> {
        &self.values
    }

    /// Decompose into `(branch_indices, values)`.
    #[inline]
    pub fn into_parts(self) -> (Vec<usize>, Mat<f64>) {
        (self.branch_indices, self.values)
    }
}

impl Index<(usize, usize)> for LodfMatrixResult {
    type Output = f64;

    fn index(&self, index: (usize, usize)) -> &Self::Output {
        &self.values[index]
    }
}

/// Sparse LODF entries keyed by `(monitored_branch_idx, outage_branch_idx)`.
#[derive(Debug, Clone, PartialEq)]
pub struct LodfPairs {
    monitored_branch_indices: Vec<usize>,
    outage_branch_indices: Vec<usize>,
    values: HashMap<(usize, usize), f64>,
}

impl LodfPairs {
    pub(crate) fn new(
        monitored_branch_indices: &[usize],
        outage_branch_indices: &[usize],
        values: HashMap<(usize, usize), f64>,
    ) -> Self {
        Self {
            monitored_branch_indices: monitored_branch_indices.to_vec(),
            outage_branch_indices: outage_branch_indices.to_vec(),
            values,
        }
    }

    /// Returns the monitored branch indices.
    #[inline]
    pub fn monitored_branches(&self) -> &[usize] {
        &self.monitored_branch_indices
    }

    /// Returns the outage branch indices.
    #[inline]
    pub fn outage_branches(&self) -> &[usize] {
        &self.outage_branch_indices
    }

    /// Number of stored LODF entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns `true` if no entries are stored.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Returns the LODF value for a `(monitored, outage)` pair, if present.
    #[inline]
    pub fn get(&self, monitored_branch_idx: usize, outage_branch_idx: usize) -> Option<f64> {
        self.values
            .get(&(monitored_branch_idx, outage_branch_idx))
            .copied()
    }

    /// Returns a reference to the underlying `(monitored, outage) -> LODF` map.
    #[inline]
    pub fn entries(&self) -> &HashMap<(usize, usize), f64> {
        &self.values
    }

    /// Decompose into `(monitored_indices, outage_indices, entries)`.
    #[allow(clippy::type_complexity)]
    #[inline]
    pub fn into_parts(self) -> (Vec<usize>, Vec<usize>, HashMap<(usize, usize), f64>) {
        (
            self.monitored_branch_indices,
            self.outage_branch_indices,
            self.values,
        )
    }
}

impl IntoIterator for LodfPairs {
    type Item = ((usize, usize), f64);
    type IntoIter = std::collections::hash_map::IntoIter<(usize, usize), f64>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.into_iter()
    }
}

impl<'a> IntoIterator for &'a LodfPairs {
    type Item = (&'a (usize, usize), &'a f64);
    type IntoIter = std::collections::hash_map::Iter<'a, (usize, usize), f64>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}

// ---------------------------------------------------------------------------
// N-2 LODF
// ---------------------------------------------------------------------------

/// N-2 LODF result for one ordered outage pair.
#[derive(Debug, Clone, PartialEq)]
pub struct N2LodfResult {
    monitored_branch_indices: Vec<usize>,
    outage_pair: (usize, usize),
    values: Vec<f64>,
}

impl N2LodfResult {
    pub(crate) fn new(
        monitored_branch_indices: &[usize],
        outage_pair: (usize, usize),
        values: Vec<f64>,
    ) -> Self {
        Self {
            monitored_branch_indices: monitored_branch_indices.to_vec(),
            outage_pair,
            values,
        }
    }

    /// Returns the monitored branch indices.
    #[inline]
    pub fn monitored_branches(&self) -> &[usize] {
        &self.monitored_branch_indices
    }

    /// Returns the `(first, second)` outage branch pair.
    #[inline]
    pub fn outage_pair(&self) -> (usize, usize) {
        self.outage_pair
    }

    /// Returns the N-2 LODF values in monitored-branch order.
    #[inline]
    pub fn values(&self) -> &[f64] {
        &self.values
    }

    /// Number of monitored branches.
    #[inline]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns `true` if no monitored branches.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Iterates over N-2 LODF values in monitored-branch order.
    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'_, f64> {
        self.values.iter()
    }

    /// Decompose into `(monitored_indices, outage_pair, values)`.
    #[inline]
    pub fn into_parts(self) -> (Vec<usize>, (usize, usize), Vec<f64>) {
        (self.monitored_branch_indices, self.outage_pair, self.values)
    }
}

impl Index<usize> for N2LodfResult {
    type Output = f64;

    fn index(&self, index: usize) -> &Self::Output {
        &self.values[index]
    }
}

/// Batched N-2 LODF result for ordered outage pairs.
#[derive(Debug, Clone, PartialEq)]
pub struct N2LodfBatchResult {
    monitored_branch_indices: Vec<usize>,
    outage_pairs: Vec<(usize, usize)>,
    values: Mat<f64>,
}

impl N2LodfBatchResult {
    pub(crate) fn new(
        monitored_branch_indices: &[usize],
        outage_pairs: &[(usize, usize)],
        values: Mat<f64>,
    ) -> Self {
        Self {
            monitored_branch_indices: monitored_branch_indices.to_vec(),
            outage_pairs: outage_pairs.to_vec(),
            values,
        }
    }

    /// Returns the monitored branch indices (row order).
    #[inline]
    pub fn monitored_branches(&self) -> &[usize] {
        &self.monitored_branch_indices
    }

    /// Returns the outage pairs (column order).
    #[inline]
    pub fn outage_pairs(&self) -> &[(usize, usize)] {
        &self.outage_pairs
    }

    /// Number of monitored branches (rows).
    #[inline]
    pub fn n_rows(&self) -> usize {
        self.values.nrows()
    }

    /// Number of outage pairs (columns).
    #[inline]
    pub fn n_cols(&self) -> usize {
        self.values.ncols()
    }

    /// Returns a reference to the underlying dense N-2 LODF matrix.
    #[inline]
    pub fn matrix(&self) -> &Mat<f64> {
        &self.values
    }

    /// Decompose into `(monitored_indices, outage_pairs, values)`.
    #[inline]
    pub fn into_parts(self) -> (Vec<usize>, Vec<(usize, usize)>, Mat<f64>) {
        (
            self.monitored_branch_indices,
            self.outage_pairs,
            self.values,
        )
    }
}

impl Index<(usize, usize)> for N2LodfBatchResult {
    type Output = f64;

    fn index(&self, index: (usize, usize)) -> &Self::Output {
        &self.values[index]
    }
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub(crate) struct BranchDcMetadata {
    pub(crate) from_full: usize,
    pub(crate) to_full: usize,
    pub(crate) b_dc: f64,
    pub(crate) in_service: bool,
    pub(crate) has_reactance: bool,
}

impl BranchDcMetadata {
    #[inline(always)]
    pub(crate) fn is_sensitivity_active(self) -> bool {
        self.in_service && self.has_reactance
    }
}
