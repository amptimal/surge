// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! DC power flow solver implementation.
//!
//! Solves the linear DC power flow equations:
//!   P = B' * theta
//!
//! where P is the vector of real power injections (per-unit),
//! B' is the susceptance matrix (with slack bus removed),
//! and theta is the vector of bus voltage angles.
//!
//! Uses sparse KLU factorization for O(n log n) performance at any scale.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use faer::Mat;
use surge_network::Network;
use surge_network::network::BusType;
use surge_network::network::apply_angle_reference_subset;
use surge_solution::{PfModel, PfSolution, SolveStatus};
use surge_sparse::KluSolver;
use surge_topology::islands::detect_islands;
use tracing::{debug, error, info, warn};

use crate::bprime::{BprimeSparseCsc, build_bprime_sparse_for_buses, build_p_injection_for_buses};
use crate::types::*;

struct PreparedDcKernel {
    bus_indices: Vec<usize>,
    branch_indices: Vec<usize>,
    bprime: BprimeSparseCsc,
    reduced_to_full: Vec<usize>,
    p_injection_base: Vec<f64>,
    reference_angle0_rad: f64,
    klu: KluSolver,
}

struct PreparedSolveInputs<'a> {
    bus_map: &'a HashMap<u32, usize>,
    bus_indices: &'a [usize],
    branch_indices: &'a [usize],
    bprime: &'a BprimeSparseCsc,
    klu: &'a mut KluSolver,
    p_inj_base: &'a [f64],
    reference_angle0_rad: f64,
}

/// Prepared DC solve model for a network.
///
/// This caches the validated topology, one reduced B' / KLU kernel per AC
/// island, and the global branch / bus metadata so callers can run `solve()`,
/// `compute_ptdf()`, and `compute_lodf()` without rebuilding the sparse model
/// each time. Single-island and multi-island networks share the same public API.
pub struct PreparedDcStudy<'a> {
    network: &'a Network,
    bus_map: HashMap<u32, usize>,
    branch_metadata: Vec<BranchDcMetadata>,
    downward_headroom_by_bus_pu: Vec<f64>,
    kernels: Vec<PreparedDcKernel>,
    bus_kernel_indices: Vec<Option<usize>>,
    branch_kernel_indices: Vec<Option<usize>>,
    bus_island_ids: Vec<usize>,
    singleton_bus_indices: Vec<usize>,
}

/// Lazily computes LODF columns from a prepared DC model.
///
/// This caches the solved reduced-bus basis columns needed to evaluate outage
/// columns, so repeated `compute_column()` calls avoid recomputing the same
/// endpoint solves. Use this for large-system workflows that need to process
/// one outage column at a time without materializing a dense all-pairs matrix.
pub struct LodfColumnBuilder<'model, 'network> {
    model: &'model mut PreparedDcStudy<'network>,
    reduced_bus_columns_by_kernel: Vec<Vec<Option<Vec<f64>>>>,
}

/// Lazily computes N-2 LODF columns for a fixed monitored/outage universe.
///
/// This builder caches the single-outage LODF columns required to evaluate
/// Woodbury rank-2 N-2 sensitivities, so repeated `compute_column()` calls for
/// large contingency studies avoid recomputing the same outage columns.
pub struct N2LodfColumnBuilder<'model, 'network> {
    monitored_branch_indices: Vec<usize>,
    column_branch_indices: Vec<usize>,
    column_positions: Vec<Option<usize>>,
    single_outage_columns: HashMap<usize, Vec<f64>>,
    lodf_columns: LodfColumnBuilder<'model, 'network>,
}

impl<'model, 'network> LodfColumnBuilder<'model, 'network> {
    fn new(model: &'model mut PreparedDcStudy<'network>) -> Self {
        let reduced_bus_columns_by_kernel = model
            .kernels
            .iter()
            .map(|kernel| vec![None; kernel.bprime.dim])
            .collect();
        Self {
            model,
            reduced_bus_columns_by_kernel,
        }
    }

    /// Compute one LODF outage column for the requested monitored branch set.
    ///
    /// The returned vector is ordered exactly like `monitored_branch_indices`.
    /// Entry `i` is `LODF[monitored_branch_indices[i], outage_branch_idx]`.
    pub fn compute_column(
        &mut self,
        monitored_branch_indices: &[usize],
        outage_branch_idx: usize,
    ) -> Result<Vec<f64>, DcError> {
        let branch_k = self.model.branch_metadata(outage_branch_idx)?;
        let (from_k, to_k) = self.model.branch_terminal_indices(outage_branch_idx)?;
        let kernel_idx = self.model.branch_kernel_index(outage_branch_idx)?;
        let b_k = branch_k.b_dc;

        let from_column = self.ensure_reduced_bus_column(kernel_idx, from_k)?;
        let to_column = self.ensure_reduced_bus_column(kernel_idx, to_k)?;

        let ptdf_k_from = b_k
            * (self.b_inverse_element(kernel_idx, from_column, from_k)
                - self.b_inverse_element(kernel_idx, from_column, to_k));
        let ptdf_k_to = b_k
            * (self.b_inverse_element(kernel_idx, to_column, from_k)
                - self.b_inverse_element(kernel_idx, to_column, to_k));
        let ptdf_kk = ptdf_k_from - ptdf_k_to;

        if (1.0 - ptdf_kk).abs() < crate::sensitivity::BRIDGE_THRESHOLD {
            return Ok(vec![f64::INFINITY; monitored_branch_indices.len()]);
        }

        let denom = 1.0 - ptdf_kk;
        let mut lodf_column = Vec::with_capacity(monitored_branch_indices.len());
        for &monitored_branch_idx in monitored_branch_indices {
            if monitored_branch_idx == outage_branch_idx {
                lodf_column.push(-1.0);
                continue;
            }

            let branch_l = self.model.branch_metadata(monitored_branch_idx)?;
            if !branch_l.is_sensitivity_active() {
                lodf_column.push(0.0);
                continue;
            }
            if self.model.branch_kernel_indices[monitored_branch_idx] != Some(kernel_idx) {
                lodf_column.push(0.0);
                continue;
            }

            let from_l = branch_l.from_full;
            let to_l = branch_l.to_full;
            let b_l = branch_l.b_dc;
            let ptdf_l_from = b_l
                * (self.b_inverse_element(kernel_idx, from_column, from_l)
                    - self.b_inverse_element(kernel_idx, from_column, to_l));
            let ptdf_l_to = b_l
                * (self.b_inverse_element(kernel_idx, to_column, from_l)
                    - self.b_inverse_element(kernel_idx, to_column, to_l));
            let ptdf_lk = ptdf_l_from - ptdf_l_to;
            lodf_column.push(ptdf_lk / denom);
        }

        Ok(lodf_column)
    }

    /// Visit a sequence of outage columns without materializing a matrix.
    pub fn stream_columns<F>(
        &mut self,
        monitored_branch_indices: &[usize],
        outage_branch_indices: &[usize],
        mut visit_column: F,
    ) -> Result<(), DcError>
    where
        F: FnMut(usize, usize, &[f64]) -> Result<(), DcError>,
    {
        for (outage_pos, &outage_branch_idx) in outage_branch_indices.iter().enumerate() {
            let column = self.compute_column(monitored_branch_indices, outage_branch_idx)?;
            visit_column(outage_pos, outage_branch_idx, &column)?;
        }
        Ok(())
    }

    fn ensure_reduced_bus_column(
        &mut self,
        kernel_idx: usize,
        full_bus_idx: usize,
    ) -> Result<Option<usize>, DcError> {
        let kernel = &self.model.kernels[kernel_idx];
        let Some(reduced_idx) = self.model.kernels[kernel_idx]
            .bprime
            .full_to_reduced
            .get(full_bus_idx)
            .copied()
            .flatten()
        else {
            return Ok(None);
        };

        if self.reduced_bus_columns_by_kernel[kernel_idx][reduced_idx].is_none() {
            let mut rhs = vec![0.0f64; kernel.bprime.dim];
            rhs[reduced_idx] = 1.0;
            if self.model.kernels[kernel_idx].klu.solve(&mut rhs).is_err() {
                return Err(DcError::SingularMatrix);
            }
            self.reduced_bus_columns_by_kernel[kernel_idx][reduced_idx] = Some(rhs);
        }

        Ok(Some(reduced_idx))
    }

    fn b_inverse_element(
        &self,
        kernel_idx: usize,
        reduced_bus_column: Option<usize>,
        full_bus_idx: usize,
    ) -> f64 {
        let kernel = &self.model.kernels[kernel_idx];
        match (
            reduced_bus_column,
            kernel
                .bprime
                .full_to_reduced
                .get(full_bus_idx)
                .copied()
                .flatten(),
        ) {
            (Some(column_idx), Some(row_idx)) => self.reduced_bus_columns_by_kernel[kernel_idx]
                [column_idx]
                .as_ref()
                .expect("reduced-bus column should exist after ensure_reduced_bus_column")[row_idx],
            _ => 0.0,
        }
    }
}

impl<'model, 'network> N2LodfColumnBuilder<'model, 'network> {
    fn new(
        model: &'model mut PreparedDcStudy<'network>,
        monitored_branch_indices: &[usize],
        candidate_outage_branch_indices: &[usize],
    ) -> Result<Self, DcError> {
        let n_branches = model.network.n_branches();
        let mut column_branch_indices = Vec::with_capacity(
            monitored_branch_indices.len() + candidate_outage_branch_indices.len(),
        );
        let mut column_positions = vec![None; n_branches];

        for &branch_idx in monitored_branch_indices {
            if branch_idx >= n_branches {
                return Err(DcError::InvalidNetwork(format!(
                    "monitored branch index {branch_idx} out of range (len={n_branches})"
                )));
            }
            if column_positions[branch_idx].is_none() {
                column_positions[branch_idx] = Some(column_branch_indices.len());
                column_branch_indices.push(branch_idx);
            }
        }

        for &branch_idx in candidate_outage_branch_indices {
            if branch_idx >= n_branches {
                return Err(DcError::InvalidNetwork(format!(
                    "candidate outage branch index {branch_idx} out of range (len={n_branches})"
                )));
            }
            if column_positions[branch_idx].is_none() {
                column_positions[branch_idx] = Some(column_branch_indices.len());
                column_branch_indices.push(branch_idx);
            }
        }

        Ok(Self {
            monitored_branch_indices: monitored_branch_indices.to_vec(),
            column_branch_indices,
            column_positions,
            single_outage_columns: HashMap::new(),
            lodf_columns: LodfColumnBuilder::new(model),
        })
    }

    /// Compute one rank-2 N-2 LODF column for an ordered outage pair.
    ///
    /// The returned vector is ordered exactly like the monitored set passed to
    /// `PreparedDcStudy::n2_lodf_columns()`. For a pair `(k, l)`, each entry is:
    ///
    /// ```text
    /// [LODF(m,k) + LODF(m,l) × LODF(l,k)] / [1 - LODF(l,k) × LODF(k,l)]
    /// ```
    ///
    /// This is the factor that multiplies the pre-outage flow on branch `k`
    /// when predicting post-N-2 flow with outages `(k, l)`.
    pub fn compute_column(
        &mut self,
        first_outage_branch_idx: usize,
        second_outage_branch_idx: usize,
    ) -> Result<Vec<f64>, DcError> {
        if first_outage_branch_idx == second_outage_branch_idx {
            return Err(DcError::Infeasible(format!(
                "N-2 outage pair must contain two distinct branches, got ({first_outage_branch_idx}, {second_outage_branch_idx})"
            )));
        }

        self.ensure_outage_column(first_outage_branch_idx)?;
        self.ensure_outage_column(second_outage_branch_idx)?;

        let first_position = self.branch_position(first_outage_branch_idx)?;
        let second_position = self.branch_position(second_outage_branch_idx)?;

        let first_column = self
            .single_outage_columns
            .get(&first_outage_branch_idx)
            .expect("outage column should exist after ensure_outage_column");
        let second_column = self
            .single_outage_columns
            .get(&second_outage_branch_idx)
            .expect("outage column should exist after ensure_outage_column");

        let lodf_second_first = first_column[second_position];
        let lodf_first_second = second_column[first_position];
        let denom = 1.0 - lodf_second_first * lodf_first_second;

        if !lodf_second_first.is_finite()
            || !lodf_first_second.is_finite()
            || !denom.is_finite()
            || denom.abs() < crate::sensitivity::BRIDGE_THRESHOLD
        {
            return Err(DcError::Infeasible(format!(
                "N-2 LODF denominator invalid for branch pair ({first_outage_branch_idx},{second_outage_branch_idx})"
            )));
        }

        let mut n2_column = Vec::with_capacity(self.monitored_branch_indices.len());
        for &monitored_branch_idx in &self.monitored_branch_indices {
            let monitored_position = self.branch_position(monitored_branch_idx)?;
            let lodf_monitored_first = first_column[monitored_position];
            let lodf_monitored_second = second_column[monitored_position];
            n2_column
                .push((lodf_monitored_first + lodf_monitored_second * lodf_second_first) / denom);
        }

        Ok(n2_column)
    }

    /// Visit a sequence of ordered N-2 outage-pair columns without materializing a matrix.
    pub fn stream_columns<F>(
        &mut self,
        outage_pairs: &[(usize, usize)],
        mut visit_column: F,
    ) -> Result<(), DcError>
    where
        F: FnMut(usize, (usize, usize), &[f64]) -> Result<(), DcError>,
    {
        for (pair_pos, &outage_pair) in outage_pairs.iter().enumerate() {
            let column = self.compute_column(outage_pair.0, outage_pair.1)?;
            visit_column(pair_pos, outage_pair, &column)?;
        }
        Ok(())
    }

    fn ensure_outage_column(&mut self, outage_branch_idx: usize) -> Result<(), DcError> {
        if self.single_outage_columns.contains_key(&outage_branch_idx) {
            return Ok(());
        }
        let Some(_) = self.column_positions.get(outage_branch_idx) else {
            return Err(DcError::InvalidNetwork(format!(
                "outage branch index {outage_branch_idx} out of range (len={})",
                self.column_positions.len()
            )));
        };
        if self.column_positions[outage_branch_idx].is_none() {
            return Err(DcError::InvalidNetwork(format!(
                "outage branch index {outage_branch_idx} not in N-2 outage universe"
            )));
        }

        let column = self
            .lodf_columns
            .compute_column(&self.column_branch_indices, outage_branch_idx)?;
        self.single_outage_columns.insert(outage_branch_idx, column);
        Ok(())
    }

    fn branch_position(&self, branch_idx: usize) -> Result<usize, DcError> {
        self.column_positions
            .get(branch_idx)
            .and_then(|pos| *pos)
            .ok_or_else(|| {
                DcError::InvalidNetwork(format!(
                    "branch index {branch_idx} not available in the N-2 column universe"
                ))
            })
    }
}

impl<'a> PreparedDcStudy<'a> {
    /// Prepare a reusable DC model for a network.
    pub fn new(network: &'a Network) -> Result<Self, DcError> {
        Self::build(network, true)
    }

    /// Prepare a reusable DC model for a structurally valid derived topology.
    ///
    /// This constructor is intended for advanced/internal workflows such as
    /// exact post-contingency fallback, where a valid base network may split
    /// into islands that no longer retain an explicit slack designation. It
    /// still validates structural integrity, but it intentionally skips the
    /// public DC solve contract that requires exactly one slack bus per
    /// connected component.
    pub fn new_unchecked(network: &'a Network) -> Result<Self, DcError> {
        Self::build(network, false)
    }

    fn build(network: &'a Network, validate_ready: bool) -> Result<Self, DcError> {
        if network.n_buses() == 0 {
            return Err(DcError::EmptyNetwork);
        }
        if validate_ready {
            network
                .validate_for_dc_solve()
                .map_err(|err| DcError::InvalidNetwork(err.to_string()))?;
        } else {
            network
                .validate_structure()
                .map_err(|err| DcError::InvalidNetwork(err.to_string()))?;
        }

        let bus_map = network.bus_index_map();
        let islands = detect_islands(network, &bus_map);
        let branch_metadata = build_branch_metadata(network, &bus_map)?;
        let downward_headroom_by_bus_pu = build_downward_headroom_by_bus(network, &bus_map);
        let mut kernels = Vec::new();
        let mut bus_kernel_indices = vec![None; network.n_buses()];
        let mut branch_kernel_indices = vec![None; network.n_branches()];
        let mut bus_island_ids = vec![0usize; network.n_buses()];
        let mut singleton_bus_indices = Vec::new();
        for (island_id, island_buses) in islands.components.iter().enumerate() {
            for &bus_idx in island_buses {
                bus_island_ids[bus_idx] = island_id;
            }
            if island_buses.len() <= 1 {
                if let Some(&bus_idx) = island_buses.first() {
                    singleton_bus_indices.push(bus_idx);
                }
                continue;
            }

            let slack_idx = choose_island_slack_idx(network, island_buses)?;
            let island_bus_set: HashSet<usize> = island_buses.iter().copied().collect();
            let branch_indices = island_branch_indices(network, &island_bus_set, &bus_map);
            let bprime = if island_buses.len() == network.n_buses() && islands.n_islands == 1 {
                build_bprime_sparse_for_buses(network, None, slack_idx)
            } else {
                build_bprime_sparse_for_buses(network, Some(&island_bus_set), slack_idx)
            };
            let p_injection_base = build_p_injection_for_buses(
                network,
                &bprime.full_to_reduced,
                slack_idx,
                Some(&island_bus_set),
            );
            let mut reduced_to_full = vec![0usize; bprime.dim];
            for (full_idx, reduced_idx) in bprime.full_to_reduced.iter().enumerate() {
                if let Some(ri) = reduced_idx {
                    reduced_to_full[*ri] = full_idx;
                }
            }
            let mut klu = KluSolver::new(bprime.dim, &bprime.col_ptrs, &bprime.row_indices)
                .map_err(|_| DcError::SingularMatrix)?;
            if klu.factor(&bprime.values).is_err() {
                return Err(DcError::SingularMatrix);
            }

            let kernel_idx = kernels.len();
            for &bus_idx in island_buses {
                bus_kernel_indices[bus_idx] = Some(kernel_idx);
            }
            for &branch_idx in &branch_indices {
                branch_kernel_indices[branch_idx] = Some(kernel_idx);
            }

            kernels.push(PreparedDcKernel {
                bus_indices: island_buses.clone(),
                branch_indices,
                bprime,
                reduced_to_full,
                p_injection_base,
                reference_angle0_rad: network.buses[slack_idx].voltage_angle_rad,
                klu,
            });
        }

        Ok(Self {
            network,
            bus_map,
            branch_metadata,
            downward_headroom_by_bus_pu,
            kernels,
            bus_kernel_indices,
            branch_kernel_indices,
            bus_island_ids,
            singleton_bus_indices,
        })
    }

    /// Solve DC power flow using the prepared model.
    pub fn solve(&mut self, opts: &DcPfOptions) -> Result<DcPfSolution, DcError> {
        let start = Instant::now();
        let mut theta_all = vec![0.0f64; self.network.n_buses()];
        let mut branch_flow_all = vec![0.0f64; self.network.n_branches()];
        let mut p_inject_all = vec![0.0f64; self.network.n_buses()];
        let mut total_slack_p = 0.0f64;
        let mut slack_distribution = HashMap::new();

        for (bus_idx, kernel_idx) in self.bus_kernel_indices.iter().enumerate() {
            if kernel_idx.is_none() {
                theta_all[bus_idx] = self.network.buses[bus_idx].voltage_angle_rad;
                apply_angle_reference_subset(
                    &mut theta_all,
                    self.network,
                    &[bus_idx],
                    bus_idx,
                    self.network.buses[bus_idx].voltage_angle_rad,
                    opts.angle_reference,
                );
            }
        }
        for &bus_idx in &self.singleton_bus_indices {
            let slack_p = compute_singleton_slack_injection_pu(self.network, bus_idx, opts);
            total_slack_p += slack_p;
            p_inject_all[bus_idx] = slack_p;
            if opts.participation_factors.is_some() || opts.headroom_slack_bus_indices.is_some() {
                slack_distribution.insert(bus_idx, slack_p * self.network.base_mva);
            }
        }

        for kernel in &mut self.kernels {
            let result = solve_prepared(
                self.network,
                opts,
                PreparedSolveInputs {
                    bus_map: &self.bus_map,
                    bus_indices: &kernel.bus_indices,
                    branch_indices: &kernel.branch_indices,
                    bprime: &kernel.bprime,
                    klu: &mut kernel.klu,
                    p_inj_base: &kernel.p_injection_base,
                    reference_angle0_rad: kernel.reference_angle0_rad,
                },
                start,
            )?;

            for &bus_idx in &kernel.bus_indices {
                theta_all[bus_idx] = result.theta[bus_idx];
                p_inject_all[bus_idx] = result.p_inject_pu[bus_idx];
            }
            for &branch_idx in &kernel.branch_indices {
                branch_flow_all[branch_idx] = result.branch_p_flow[branch_idx];
            }

            total_slack_p += result.slack_p_injection;
            slack_distribution.extend(result.slack_distribution);
        }

        let solve_time = start.elapsed().as_secs_f64();
        Ok(DcPfSolution {
            theta: theta_all,
            branch_p_flow: branch_flow_all,
            slack_p_injection: total_slack_p,
            solve_time_secs: solve_time,
            total_generation_mw: if opts.participation_factors.is_some()
                || opts.headroom_slack_bus_indices.is_some()
            {
                self.network.total_load_mw() + total_slack_p * self.network.base_mva
            } else {
                0.0
            },
            slack_distribution,
            p_inject_pu: p_inject_all,
            island_ids: self.compute_island_ids(),
        })
    }

    fn compute_island_ids(&self) -> Vec<usize> {
        self.bus_island_ids.clone()
    }

    /// Compute the canonical DC power flow + analysis workflow over one prepared study.
    pub fn run_analysis(
        &mut self,
        request: &DcAnalysisRequest,
    ) -> Result<DcAnalysisResult, DcError> {
        let monitored_branch_indices = request
            .monitored_branch_indices
            .clone()
            .unwrap_or_else(|| (0..self.network.n_branches()).collect());
        let sensitivity_options = request.sensitivity_options.clone().unwrap_or_else(|| {
            if let Some(pf_weights) = request.pf_options.participation_factors.as_ref() {
                crate::sensitivity::DcSensitivityOptions::with_slack_weights(pf_weights)
            } else if let Some(bus_indices) = request.pf_options.headroom_slack_bus_indices.as_ref()
            {
                crate::sensitivity::DcSensitivityOptions::with_headroom_slack(bus_indices)
            } else {
                crate::sensitivity::DcSensitivityOptions::default()
            }
        });
        let normalized_slack =
            self.resolve_sensitivity_slack_by_kernel(&sensitivity_options.slack)?;
        let ptdf_bus_indices =
            self.resolve_selected_bus_indices(request.ptdf_bus_indices.as_deref())?;
        let power_flow = self.solve(&request.pf_options)?;
        let ptdf = self.compute_ptdf_with_resolved_slack(
            &monitored_branch_indices,
            Some(&ptdf_bus_indices),
            &normalized_slack,
        )?;
        let otdf_bus_indices = if request.otdf_outage_branch_indices.is_empty() {
            Vec::new()
        } else {
            request
                .otdf_bus_indices
                .clone()
                .unwrap_or_else(|| (0..self.network.n_buses()).collect())
        };
        let otdf = if request.otdf_outage_branch_indices.is_empty() {
            None
        } else {
            Some(self.compute_otdf_with_resolved_slack(
                &monitored_branch_indices,
                &request.otdf_outage_branch_indices,
                Some(&otdf_bus_indices),
                &normalized_slack,
            )?)
        };
        let lodf = if request.lodf_outage_branch_indices.is_empty() {
            None
        } else {
            Some(self.compute_lodf(
                &monitored_branch_indices,
                &request.lodf_outage_branch_indices,
            )?)
        };
        let n2_lodf = if request.n2_outage_pairs.is_empty() {
            None
        } else {
            Some(self.compute_n2_lodf_batch(&request.n2_outage_pairs, &monitored_branch_indices)?)
        };

        Ok(DcAnalysisResult {
            power_flow,
            monitored_branch_indices,
            ptdf,
            ptdf_bus_indices,
            otdf,
            otdf_outage_branch_indices: request.otdf_outage_branch_indices.clone(),
            otdf_bus_indices,
            lodf,
            lodf_outage_branch_indices: request.lodf_outage_branch_indices.clone(),
            n2_lodf,
            n2_outage_pairs: request.n2_outage_pairs.clone(),
        })
    }

    /// Compute PTDF rows for the given monitored branches using single-slack semantics.
    pub fn compute_ptdf(
        &mut self,
        monitored_branch_indices: &[usize],
    ) -> Result<PtdfRows, DcError> {
        self.compute_ptdf_with_resolved_slack(
            monitored_branch_indices,
            None,
            &self.single_slack_by_kernel(),
        )
    }

    /// Compute DC marginal loss sensitivities without materializing a loss PTDF.
    ///
    /// This returns the same single-slack-gauge vector as
    /// `Σ_l 2 * r_l * flow_l * PTDF[l, bus]`, but evaluates that product with
    /// one sparse adjoint solve per island instead of a dense branch-by-bus PTDF
    /// cache. `theta` is indexed by the network's internal bus order.
    pub fn compute_loss_sensitivities_adjoint(
        &mut self,
        theta: &[f64],
    ) -> Result<Vec<f64>, DcError> {
        let n_bus = self.network.n_buses();
        if theta.len() != n_bus {
            return Err(DcError::InvalidNetwork(format!(
                "loss sensitivity theta length {} does not match network bus count {}",
                theta.len(),
                n_bus
            )));
        }

        let mut rhs_by_kernel: Vec<Vec<f64>> = self
            .kernels
            .iter()
            .map(|kernel| vec![0.0_f64; kernel.bprime.dim])
            .collect();

        for (branch_idx, branch) in self.network.branches.iter().enumerate() {
            let branch_meta = self.branch_metadata(branch_idx)?;
            if !branch_meta.is_sensitivity_active() {
                continue;
            }
            if branch.r.abs() < 1e-20 {
                continue;
            }
            let Some(kernel_idx) = self.branch_kernel_indices[branch_idx] else {
                continue;
            };

            let flow_pu =
                branch_meta.b_dc * (theta[branch_meta.from_full] - theta[branch_meta.to_full]);
            let weight = 2.0 * branch.r * flow_pu * branch_meta.b_dc;
            if weight.abs() < 1e-20 {
                continue;
            }

            let kernel = &self.kernels[kernel_idx];
            let rhs = &mut rhs_by_kernel[kernel_idx];
            if let Some(ri_from) = kernel.bprime.full_to_reduced[branch_meta.from_full] {
                rhs[ri_from] += weight;
            }
            if let Some(ri_to) = kernel.bprime.full_to_reduced[branch_meta.to_full] {
                rhs[ri_to] -= weight;
            }
        }

        let mut dloss_dp = vec![0.0_f64; n_bus];
        for (kernel_idx, kernel) in self.kernels.iter_mut().enumerate() {
            let rhs = &mut rhs_by_kernel[kernel_idx];
            if rhs.is_empty() {
                continue;
            }
            kernel.klu.solve(rhs).map_err(|_| DcError::SingularMatrix)?;
            for (ri, &full_bus_idx) in kernel.reduced_to_full.iter().enumerate() {
                dloss_dp[full_bus_idx] = rhs[ri];
            }
            // The reduced slack row/column is absent, so the slack bus remains
            // zero. That matches the default single-slack PTDF semantics.
        }

        Ok(dloss_dp)
    }

    /// Compute PTDF rows from an explicit [`PtdfRequest`](crate::PtdfRequest).
    pub fn compute_ptdf_request(
        &mut self,
        request: &crate::sensitivity::PtdfRequest,
    ) -> Result<PtdfRows, DcError> {
        let monitored_branch_indices_storage;
        let monitored_branch_indices = if let Some(indices) =
            request.monitored_branch_indices.as_deref()
        {
            indices
        } else {
            monitored_branch_indices_storage = (0..self.network.n_branches()).collect::<Vec<_>>();
            &monitored_branch_indices_storage
        };
        self.compute_ptdf_with_options(
            monitored_branch_indices,
            request.bus_indices.as_deref(),
            &request.options,
        )
    }

    fn compute_ptdf_with_resolved_slack(
        &mut self,
        monitored_branch_indices: &[usize],
        bus_indices: Option<&[usize]>,
        normalized_slack_by_kernel: &[Option<Vec<(usize, f64)>>],
    ) -> Result<PtdfRows, DcError> {
        let selected_bus_indices = self.resolve_selected_bus_indices(bus_indices)?;
        let n_branches = self.network.n_branches();
        let mut result = PtdfRows::new(
            monitored_branch_indices,
            &selected_bus_indices,
            n_branches,
            self.network.n_buses(),
        );
        let mut monitored_by_kernel = vec![Vec::<(usize, usize)>::new(); self.kernels.len()];
        for (row_pos, &branch_idx) in monitored_branch_indices.iter().enumerate() {
            let branch = self.branch_metadata(branch_idx)?;
            if !branch.is_sensitivity_active() {
                continue;
            }
            if let Some(kernel_idx) = self.branch_kernel_indices[branch_idx] {
                monitored_by_kernel[kernel_idx].push((row_pos, branch_idx));
            }
        }
        let mut bus_positions_by_kernel = vec![Vec::<(usize, usize)>::new(); self.kernels.len()];
        for (bus_pos, &bus_idx) in selected_bus_indices.iter().enumerate() {
            if let Some(kernel_idx) = self.bus_kernel_indices[bus_idx] {
                bus_positions_by_kernel[kernel_idx].push((bus_pos, bus_idx));
            }
        }

        for kernel_idx in 0..self.kernels.len() {
            if monitored_by_kernel[kernel_idx].is_empty() {
                continue;
            }

            let local_monitored: Vec<usize> = monitored_by_kernel[kernel_idx]
                .iter()
                .map(|&(_, branch_idx)| branch_idx)
                .collect();
            let local_buses: Vec<usize> = bus_positions_by_kernel[kernel_idx]
                .iter()
                .map(|&(_, bus_idx)| bus_idx)
                .collect();
            let local_ptdf = self.compute_kernel_ptdf_with_resolved_slack(
                kernel_idx,
                &local_monitored,
                Some(&local_buses),
                normalized_slack_by_kernel
                    .get(kernel_idx)
                    .and_then(|weights| weights.as_deref()),
            )?;
            for (local_row_pos, &(global_row_pos, _)) in
                monitored_by_kernel[kernel_idx].iter().enumerate()
            {
                let source_row = local_ptdf.row_at(local_row_pos);
                let target_row = result.row_at_mut(global_row_pos);
                for (local_bus_pos, &(global_bus_pos, _)) in
                    bus_positions_by_kernel[kernel_idx].iter().enumerate()
                {
                    target_row[global_bus_pos] = source_row[local_bus_pos];
                }
            }
        }

        Ok(result)
    }

    fn compute_kernel_ptdf_with_resolved_slack(
        &mut self,
        kernel_idx: usize,
        monitored_branch_indices: &[usize],
        bus_indices: Option<&[usize]>,
        normalized_slack: Option<&[(usize, f64)]>,
    ) -> Result<PtdfRows, DcError> {
        let uses_full_bus_axis = bus_indices.is_none();
        let mut active_branches = Vec::with_capacity(monitored_branch_indices.len());
        for (row_pos, &branch_idx) in monitored_branch_indices.iter().enumerate() {
            let branch = self.branch_metadata(branch_idx)?;
            if !branch.is_sensitivity_active() {
                continue;
            }
            active_branches.push((row_pos, branch.b_dc, branch.from_full, branch.to_full));
        }
        let kernel = &mut self.kernels[kernel_idx];
        let selected_bus_indices = bus_indices
            .map(|indices| indices.to_vec())
            .unwrap_or_else(|| kernel.bus_indices.clone());
        let mut result = PtdfRows::new(
            monitored_branch_indices,
            &selected_bus_indices,
            self.network.n_branches(),
            self.network.n_buses(),
        );

        const PTDF_SOLVE_BATCH_SIZE: usize = 128;
        for branch_chunk in active_branches.chunks(PTDF_SOLVE_BATCH_SIZE) {
            let chunk_len = branch_chunk.len();
            let mut rhs_batch = vec![0.0f64; kernel.bprime.dim * chunk_len];

            for (chunk_pos, &(_, _, from_full, to_full)) in branch_chunk.iter().enumerate() {
                let rhs = &mut rhs_batch
                    [chunk_pos * kernel.bprime.dim..(chunk_pos + 1) * kernel.bprime.dim];
                if let Some(ri_from) = kernel.bprime.full_to_reduced[from_full] {
                    rhs[ri_from] = 1.0;
                }
                if let Some(ri_to) = kernel.bprime.full_to_reduced[to_full] {
                    rhs[ri_to] -= 1.0;
                }
            }

            kernel
                .klu
                .solve_many(&mut rhs_batch, chunk_len)
                .map_err(|_| DcError::SingularMatrix)?;

            for (chunk_pos, &(row_pos, branch_b, _, _)) in branch_chunk.iter().enumerate() {
                let rhs =
                    &rhs_batch[chunk_pos * kernel.bprime.dim..(chunk_pos + 1) * kernel.bprime.dim];
                let row = result.row_at_mut(row_pos);
                let correction = normalized_slack
                    .map(|participants| {
                        participants
                            .iter()
                            .map(|&(bus_idx, weight)| {
                                let raw_value = kernel.bprime.full_to_reduced[bus_idx]
                                    .map(|ri| branch_b * rhs[ri])
                                    .unwrap_or(0.0);
                                raw_value * weight
                            })
                            .sum()
                    })
                    .unwrap_or(0.0);
                if uses_full_bus_axis {
                    for (ri, &full_bus_idx) in kernel.reduced_to_full.iter().enumerate() {
                        row[full_bus_idx] = branch_b * rhs[ri] - correction;
                    }
                    if normalized_slack.is_some() && kernel.bprime.slack_idx < row.len() {
                        row[kernel.bprime.slack_idx] = -correction;
                    }
                } else {
                    for (bus_pos, &full_bus_idx) in selected_bus_indices.iter().enumerate() {
                        row[bus_pos] = kernel.bprime.full_to_reduced[full_bus_idx]
                            .map(|ri| branch_b * rhs[ri] - correction)
                            .unwrap_or(-correction);
                    }
                }
            }
        }

        Ok(result)
    }

    pub fn compute_ptdf_with_options(
        &mut self,
        monitored_branch_indices: &[usize],
        bus_indices: Option<&[usize]>,
        options: &crate::sensitivity::DcSensitivityOptions,
    ) -> Result<PtdfRows, DcError> {
        let normalized_slack = self.resolve_sensitivity_slack_by_kernel(&options.slack)?;
        self.compute_ptdf_with_resolved_slack(
            monitored_branch_indices,
            bus_indices,
            &normalized_slack,
        )
    }

    /// Compute OTDF for the given monitored and outage branches using single-slack semantics.
    pub fn compute_otdf(
        &mut self,
        monitored_branch_indices: &[usize],
        outage_branch_indices: &[usize],
    ) -> Result<OtdfResult, DcError> {
        let all_bus_indices: Vec<usize> = (0..self.network.n_buses()).collect();
        self.compute_otdf_with_resolved_slack(
            monitored_branch_indices,
            outage_branch_indices,
            Some(&all_bus_indices),
            &self.single_slack_by_kernel(),
        )
    }

    /// Compute OTDF from an explicit [`OtdfRequest`](crate::OtdfRequest).
    pub fn compute_otdf_request(
        &mut self,
        request: &crate::sensitivity::OtdfRequest,
    ) -> Result<OtdfResult, DcError> {
        self.compute_otdf_with_options(
            &request.monitored_branch_indices,
            &request.outage_branch_indices,
            request.bus_indices.as_deref(),
            &request.options,
        )
    }

    pub(crate) fn compute_otdf_with_options(
        &mut self,
        monitored_branch_indices: &[usize],
        outage_branch_indices: &[usize],
        bus_indices: Option<&[usize]>,
        options: &crate::sensitivity::DcSensitivityOptions,
    ) -> Result<OtdfResult, DcError> {
        let normalized_slack = self.resolve_sensitivity_slack_by_kernel(&options.slack)?;
        self.compute_otdf_with_resolved_slack(
            monitored_branch_indices,
            outage_branch_indices,
            bus_indices,
            &normalized_slack,
        )
    }

    fn compute_otdf_with_resolved_slack(
        &mut self,
        monitored_branch_indices: &[usize],
        outage_branch_indices: &[usize],
        bus_indices: Option<&[usize]>,
        normalized_slack_by_kernel: &[Option<Vec<(usize, f64)>>],
    ) -> Result<OtdfResult, DcError> {
        let selected_bus_indices =
            crate::sensitivity::collect_selected_bus_indices(self.network, bus_indices)?;
        let union_branch_indices = crate::sensitivity::collect_unique_branch_indices(
            self.network.n_branches(),
            monitored_branch_indices,
            outage_branch_indices,
        )?;
        let ptdf = self.compute_ptdf_with_resolved_slack(
            &union_branch_indices,
            None,
            normalized_slack_by_kernel,
        )?;
        crate::sensitivity::compute_otdf_from_ptdf(
            self.network,
            monitored_branch_indices,
            outage_branch_indices,
            &selected_bus_indices,
            &ptdf,
        )
    }

    #[allow(clippy::type_complexity)]
    fn resolve_sensitivity_slack_by_kernel(
        &self,
        slack: &crate::sensitivity::DcSensitivitySlack,
    ) -> Result<Vec<Option<Vec<(usize, f64)>>>, DcError> {
        let mut per_kernel = vec![None; self.kernels.len()];
        match slack {
            crate::sensitivity::DcSensitivitySlack::SingleSlack => Ok(per_kernel),
            crate::sensitivity::DcSensitivitySlack::SlackWeights(slack_weights) => {
                let mut grouped_weights = vec![Vec::new(); self.kernels.len()];
                for &(bus_idx, weight) in slack_weights {
                    if bus_idx >= self.network.n_buses() {
                        return Err(DcError::InvalidNetwork(format!(
                            "slack weight bus index {bus_idx} out of range for network with {} buses",
                            self.network.n_buses()
                        )));
                    }
                    if let Some(kernel_idx) = self.bus_kernel_indices[bus_idx] {
                        grouped_weights[kernel_idx].push((bus_idx, weight));
                    }
                }
                for (kernel_idx, weights) in grouped_weights.into_iter().enumerate() {
                    let normalized = self.normalize_sensitivity_slack_weights(weights)?;
                    if !normalized.is_empty() {
                        per_kernel[kernel_idx] = Some(normalized);
                    }
                }
                Ok(per_kernel)
            }
            crate::sensitivity::DcSensitivitySlack::HeadroomSlack(participating_bus_indices) => {
                let mut grouped_weights = vec![Vec::new(); self.kernels.len()];
                for &bus_idx in participating_bus_indices {
                    if bus_idx >= self.network.n_buses() {
                        return Err(DcError::InvalidNetwork(format!(
                            "participating bus index {bus_idx} out of range for network with {} buses",
                            self.network.n_buses()
                        )));
                    }
                    let Some(kernel_idx) = self.bus_kernel_indices[bus_idx] else {
                        continue;
                    };
                    if bus_idx == self.kernels[kernel_idx].bprime.slack_idx {
                        continue;
                    }
                    let headroom = self.downward_headroom_by_bus_pu[bus_idx];
                    if headroom > 0.0 {
                        grouped_weights[kernel_idx].push((bus_idx, headroom));
                    }
                }
                for (kernel_idx, weights) in grouped_weights.into_iter().enumerate() {
                    let normalized = self.normalize_sensitivity_slack_weights(weights)?;
                    if !normalized.is_empty() {
                        per_kernel[kernel_idx] = Some(normalized);
                    }
                }
                Ok(per_kernel)
            }
        }
    }

    fn normalize_sensitivity_slack_weights<I>(
        &self,
        slack_weights: I,
    ) -> Result<Vec<(usize, f64)>, DcError>
    where
        I: IntoIterator<Item = (usize, f64)>,
    {
        let n_buses = self.network.n_buses();
        let mut normalized_weights: HashMap<usize, f64> = HashMap::new();
        let mut total_weight = 0.0;

        for (bus_idx, weight) in slack_weights {
            if bus_idx >= n_buses {
                return Err(DcError::InvalidNetwork(format!(
                    "slack weight bus index {bus_idx} out of range for network with {n_buses} buses"
                )));
            }
            if !weight.is_finite() {
                return Err(DcError::InvalidNetwork(format!(
                    "slack weight for bus {bus_idx} must be finite"
                )));
            }
            let clipped_weight = weight.max(0.0);
            if clipped_weight > 0.0 {
                *normalized_weights.entry(bus_idx).or_insert(0.0) += clipped_weight;
            }
            total_weight += clipped_weight;
        }

        if total_weight < 1e-12 {
            return Ok(Vec::new());
        }

        let mut normalized_weights: Vec<(usize, f64)> = normalized_weights
            .into_iter()
            .map(|(bus_idx, weight)| (bus_idx, weight / total_weight))
            .collect();
        normalized_weights.sort_unstable_by_key(|(bus_idx, _)| *bus_idx);
        Ok(normalized_weights)
    }

    fn resolve_selected_bus_indices(
        &self,
        bus_indices: Option<&[usize]>,
    ) -> Result<Vec<usize>, DcError> {
        crate::sensitivity::collect_selected_bus_indices(self.network, bus_indices)
    }

    fn single_slack_by_kernel(&self) -> Vec<Option<Vec<(usize, f64)>>> {
        vec![None; self.kernels.len()]
    }

    fn branch_kernel_index(&self, branch_idx: usize) -> Result<usize, DcError> {
        self.branch_kernel_indices
            .get(branch_idx)
            .copied()
            .flatten()
            .ok_or_else(|| {
                DcError::Infeasible(format!(
                    "branch {branch_idx} does not belong to an active prepared DC island"
                ))
            })
    }

    /// Create a lazily-evaluated LODF column builder backed by this prepared model.
    pub fn lodf_columns(&mut self) -> LodfColumnBuilder<'_, 'a> {
        LodfColumnBuilder::new(self)
    }

    /// Create a lazily-evaluated N-2 LODF column builder backed by this prepared model.
    pub fn n2_lodf_columns(
        &mut self,
        monitored_branch_indices: &[usize],
        candidate_outage_branch_indices: &[usize],
    ) -> Result<N2LodfColumnBuilder<'_, 'a>, DcError> {
        N2LodfColumnBuilder::new(
            self,
            monitored_branch_indices,
            candidate_outage_branch_indices,
        )
    }

    /// Compute rectangular LODF for the given monitored and outage branch sets.
    pub fn compute_lodf(
        &mut self,
        monitored_branch_indices: &[usize],
        outage_branch_indices: &[usize],
    ) -> Result<LodfResult, DcError> {
        let mut lodf =
            Mat::<f64>::zeros(monitored_branch_indices.len(), outage_branch_indices.len());
        let mut lodf_columns = self.lodf_columns();
        lodf_columns.stream_columns(
            monitored_branch_indices,
            outage_branch_indices,
            |outage_pos, _outage_branch_idx, column| {
                for (monitored_pos, &value) in column.iter().enumerate() {
                    lodf[(monitored_pos, outage_pos)] = value;
                }
                Ok(())
            },
        )?;
        Ok(LodfResult::new(
            monitored_branch_indices,
            outage_branch_indices,
            lodf,
        ))
    }

    /// Compute LODF from an explicit [`LodfRequest`](crate::LodfRequest).
    pub fn compute_lodf_request(
        &mut self,
        request: &crate::sensitivity::LodfRequest,
    ) -> Result<LodfResult, DcError> {
        let monitored_branch_indices_storage;
        let monitored_branch_indices = if let Some(indices) =
            request.monitored_branch_indices.as_deref()
        {
            indices
        } else {
            monitored_branch_indices_storage = (0..self.network.n_branches()).collect::<Vec<_>>();
            &monitored_branch_indices_storage
        };
        let outage_branch_indices = request
            .outage_branch_indices
            .as_deref()
            .unwrap_or(monitored_branch_indices);
        self.compute_lodf(monitored_branch_indices, outage_branch_indices)
    }

    /// Compute selected LODF entries as a sparse pair map.
    ///
    /// The returned map is keyed as `(monitored_branch_idx, outage_branch_idx)`.
    pub fn compute_lodf_pairs(
        &mut self,
        monitored_branch_indices: &[usize],
        outage_branch_indices: &[usize],
    ) -> Result<LodfPairs, DcError> {
        let mut pairs =
            HashMap::with_capacity(monitored_branch_indices.len() * outage_branch_indices.len());
        let mut lodf_columns = self.lodf_columns();
        lodf_columns.stream_columns(
            monitored_branch_indices,
            outage_branch_indices,
            |_, outage_branch_idx, column| {
                for (monitored_pos, &monitored_branch_idx) in
                    monitored_branch_indices.iter().enumerate()
                {
                    pairs.insert(
                        (monitored_branch_idx, outage_branch_idx),
                        column[monitored_pos],
                    );
                }
                Ok(())
            },
        )?;
        Ok(LodfPairs::new(
            monitored_branch_indices,
            outage_branch_indices,
            pairs,
        ))
    }

    /// Compute a dense all-pairs LODF matrix for the given branch set.
    pub fn compute_lodf_matrix(&mut self, branches: &[usize]) -> Result<LodfMatrixResult, DcError> {
        let lodf = self.compute_lodf(branches, branches)?;
        Ok(LodfMatrixResult::new(branches, lodf.into_parts().2))
    }

    /// Compute a dense all-pairs LODF matrix from an explicit [`LodfMatrixRequest`](crate::LodfMatrixRequest).
    pub fn compute_lodf_matrix_request(
        &mut self,
        request: &crate::sensitivity::LodfMatrixRequest,
    ) -> Result<LodfMatrixResult, DcError> {
        let branch_indices_storage;
        let branch_indices = if let Some(indices) = request.branch_indices.as_deref() {
            indices
        } else {
            branch_indices_storage = (0..self.network.n_branches()).collect::<Vec<_>>();
            &branch_indices_storage
        };
        self.compute_lodf_matrix(branch_indices)
    }

    /// Compute N-2 LODF for a single simultaneous double-outage pair.
    pub fn compute_n2_lodf(
        &mut self,
        outage_pair: (usize, usize),
        monitored_branch_indices: &[usize],
    ) -> Result<N2LodfResult, DcError> {
        let (k, l) = outage_pair;
        let n_br = self.network.n_branches();

        if k >= n_br || l >= n_br {
            return Err(DcError::Infeasible(format!(
                "outage_pair ({k}, {l}) out of range for network with {n_br} branches"
            )));
        }
        if k == l {
            return Err(DcError::Infeasible(format!(
                "N-2 outage pair must contain two distinct branches, got ({k}, {l})"
            )));
        }
        let mut n2_columns = self.n2_lodf_columns(monitored_branch_indices, &[k, l])?;
        let values = n2_columns.compute_column(k, l)?;
        Ok(N2LodfResult::new(
            monitored_branch_indices,
            outage_pair,
            values,
        ))
    }

    /// Compute N-2 LODF from an explicit [`N2LodfRequest`](crate::N2LodfRequest).
    pub fn compute_n2_lodf_request(
        &mut self,
        request: &crate::sensitivity::N2LodfRequest,
    ) -> Result<N2LodfResult, DcError> {
        let monitored_branch_indices_storage;
        let monitored_branch_indices = if let Some(indices) =
            request.monitored_branch_indices.as_deref()
        {
            indices
        } else {
            monitored_branch_indices_storage = (0..self.network.n_branches()).collect::<Vec<_>>();
            &monitored_branch_indices_storage
        };
        self.compute_n2_lodf(request.outage_pair, monitored_branch_indices)
    }

    /// Compute batched N-2 LODF for multiple simultaneous double-outage pairs.
    pub fn compute_n2_lodf_batch(
        &mut self,
        outage_pairs: &[(usize, usize)],
        monitored_branch_indices: &[usize],
    ) -> Result<N2LodfBatchResult, DcError> {
        let mut unique_outages = Vec::with_capacity(outage_pairs.len() * 2);
        for &(first, second) in outage_pairs {
            unique_outages.push(first);
            unique_outages.push(second);
        }
        unique_outages.sort_unstable();
        unique_outages.dedup();

        let mut n2_columns = self.n2_lodf_columns(monitored_branch_indices, &unique_outages)?;
        let mut result = Mat::<f64>::zeros(monitored_branch_indices.len(), outage_pairs.len());
        n2_columns.stream_columns(outage_pairs, |pair_pos, _pair, column| {
            for (monitored_pos, &value) in column.iter().enumerate() {
                result[(monitored_pos, pair_pos)] = value;
            }
            Ok(())
        })?;
        Ok(N2LodfBatchResult::new(
            monitored_branch_indices,
            outage_pairs,
            result,
        ))
    }

    /// Compute batched N-2 LODF from an explicit [`N2LodfBatchRequest`](crate::N2LodfBatchRequest).
    pub fn compute_n2_lodf_batch_request(
        &mut self,
        request: &crate::sensitivity::N2LodfBatchRequest,
    ) -> Result<N2LodfBatchResult, DcError> {
        let monitored_branch_indices_storage;
        let monitored_branch_indices = if let Some(indices) =
            request.monitored_branch_indices.as_deref()
        {
            indices
        } else {
            monitored_branch_indices_storage = (0..self.network.n_branches()).collect::<Vec<_>>();
            &monitored_branch_indices_storage
        };
        self.compute_n2_lodf_batch(&request.outage_pairs, monitored_branch_indices)
    }

    fn branch_terminal_indices(&self, branch_idx: usize) -> Result<(usize, usize), DcError> {
        let branch = self.branch_metadata(branch_idx)?;
        if !branch.in_service {
            return Err(DcError::Infeasible(format!(
                "branch {branch_idx} is out of service and cannot be used as an outage"
            )));
        }
        if !branch.has_reactance {
            return Err(DcError::Infeasible(format!(
                "branch {branch_idx} has near-zero reactance and cannot be used as an outage"
            )));
        }
        Ok((branch.from_full, branch.to_full))
    }

    #[inline(always)]
    fn branch_metadata(&self, branch_idx: usize) -> Result<BranchDcMetadata, DcError> {
        self.branch_metadata
            .get(branch_idx)
            .copied()
            .ok_or_else(|| {
                DcError::InvalidNetwork(format!(
                    "branch index {branch_idx} out of range (len={})",
                    self.network.branches.len()
                ))
            })
    }
}

fn compute_singleton_slack_injection_pu(
    network: &Network,
    bus_idx: usize,
    opts: &DcPfOptions,
) -> f64 {
    let mut full_to_reduced = vec![None; network.n_buses()];
    full_to_reduced[bus_idx] = Some(0);
    let included_buses = HashSet::from([bus_idx]);
    let effective_injection =
        build_p_injection_for_buses(network, &full_to_reduced, usize::MAX, Some(&included_buses));
    let bus_number = network.buses[bus_idx].number;
    let external_p_pu: f64 = opts
        .external_p_injections_mw
        .iter()
        .filter(|(target_bus, _)| *target_bus == bus_number)
        .map(|(_, p_mw)| *p_mw / network.base_mva)
        .sum();
    -(effective_injection[0] + external_p_pu)
}

/// Solve DC power flow for a network using default options (single slack).
///
/// Returns bus angles and branch flows. The slack bus angle is fixed at 0.
///
/// # Example
///
/// ```no_run
/// use surge_io::load;
/// use surge_dc::solve_dc;
///
/// let net = load("examples/cases/ieee118/case118.surge.json.zst").unwrap();
/// let sol = solve_dc(&net).unwrap();
/// println!("slack={:.2} MW, branches={}", sol.slack_p_injection, sol.branch_p_flow.len());
/// ```
pub fn solve_dc(network: &Network) -> Result<DcPfSolution, DcError> {
    solve_dc_opts(network, &DcPfOptions::default())
}

/// Solve DC power flow for a network with explicit options.
///
/// Supports PST-corrected injections (always active when `Branch::shift != 0`)
/// and optional headroom-limited slack balancing (PST-02).
///
/// When `opts.headroom_slack_bus_indices` is `Some(buses)`, the total power
/// imbalance absorbed by the single slack bus in the first solve is
/// redistributed across the listed buses in proportion to available generator
/// headroom at those buses. The B' system is re-solved once with the adjusted
/// injection vector so that angles and flows reflect the headroom-limited
/// balancing. The result includes `total_generation_mw` and
/// `slack_distribution` showing each bus's share of the absorbed mismatch.
pub fn solve_dc_opts(network: &Network, opts: &DcPfOptions) -> Result<DcPfSolution, DcError> {
    let mut study = PreparedDcStudy::new(network)?;
    study.solve(opts)
}

/// Compute DC marginal loss sensitivities with one sparse adjoint solve.
///
/// Equivalent to multiplying branch loss gradients by the single-slack PTDF,
/// but does not allocate a dense branch-by-bus PTDF matrix.
pub fn compute_loss_sensitivities_adjoint(
    network: &Network,
    theta: &[f64],
) -> Result<Vec<f64>, DcError> {
    let mut study = PreparedDcStudy::new(network)?;
    study.compute_loss_sensitivities_adjoint(theta)
}

/// Compute the canonical DC power flow + sensitivity workflow for one network.
pub fn run_dc_analysis(
    network: &Network,
    request: &DcAnalysisRequest,
) -> Result<DcAnalysisResult, DcError> {
    let mut study = PreparedDcStudy::new(network)?;
    study.run_analysis(request)
}

fn build_branch_metadata(
    network: &Network,
    bus_map: &HashMap<u32, usize>,
) -> Result<Vec<BranchDcMetadata>, DcError> {
    network
        .branches
        .iter()
        .enumerate()
        .map(|(branch_idx, branch)| {
            let from_full = bus_map.get(&branch.from_bus).copied().ok_or_else(|| {
                DcError::InvalidNetwork(format!(
                    "branch {branch_idx} references unknown from_bus {}",
                    branch.from_bus
                ))
            })?;
            let to_full = bus_map.get(&branch.to_bus).copied().ok_or_else(|| {
                DcError::InvalidNetwork(format!(
                    "branch {branch_idx} references unknown to_bus {}",
                    branch.to_bus
                ))
            })?;
            Ok(BranchDcMetadata {
                from_full,
                to_full,
                b_dc: if branch.x.abs() >= crate::types::MIN_REACTANCE {
                    branch.b_dc()
                } else {
                    0.0
                },
                in_service: branch.in_service,
                has_reactance: branch.x.abs() >= crate::types::MIN_REACTANCE,
            })
        })
        .collect()
}

fn build_downward_headroom_by_bus(network: &Network, bus_map: &HashMap<u32, usize>) -> Vec<f64> {
    let mut downward_headroom_by_bus_pu = vec![0.0; network.n_buses()];
    for generator in network
        .generators
        .iter()
        .filter(|generator| generator.in_service)
    {
        if let Some(&bus_idx) = bus_map.get(&generator.bus) {
            downward_headroom_by_bus_pu[bus_idx] +=
                (generator.p - generator.pmin).max(0.0) / network.base_mva;
        }
    }
    downward_headroom_by_bus_pu
}

fn choose_island_slack_idx(network: &Network, island_buses: &[usize]) -> Result<usize, DcError> {
    island_buses
        .iter()
        .copied()
        .find(|&idx| network.buses[idx].bus_type == BusType::Slack)
        .or_else(|| {
            island_buses
                .iter()
                .copied()
                .find(|&idx| network.buses[idx].bus_type == BusType::PV)
        })
        .or_else(|| island_buses.first().copied())
        .ok_or(DcError::NoSlackBus)
}

fn island_branch_indices(
    network: &Network,
    island_bus_set: &HashSet<usize>,
    bus_map: &HashMap<u32, usize>,
) -> Vec<usize> {
    network
        .branches
        .iter()
        .enumerate()
        .filter_map(|(idx, branch)| {
            if !branch.in_service || branch.x.abs() < crate::types::MIN_REACTANCE {
                return None;
            }
            let from_idx = *bus_map.get(&branch.from_bus)?;
            let to_idx = *bus_map.get(&branch.to_bus)?;
            (island_bus_set.contains(&from_idx) && island_bus_set.contains(&to_idx)).then_some(idx)
        })
        .collect()
}

fn solve_prepared(
    network: &Network,
    opts: &DcPfOptions,
    mut prepared: PreparedSolveInputs<'_>,
    start: Instant,
) -> Result<DcPfSolution, DcError> {
    let n = network.n_buses();
    let slack_idx = prepared.bprime.slack_idx;

    // Build effective injection vector: base + external P injections.
    let effective_p_inj: Vec<f64> = if opts.external_p_injections_mw.is_empty() {
        prepared.p_inj_base.to_vec()
    } else {
        let mut p = prepared.p_inj_base.to_vec();
        for &(bus_number, p_mw) in &opts.external_p_injections_mw {
            if let Some(&full_idx) = prepared.bus_map.get(&bus_number) {
                if let Some(ri) = prepared.bprime.full_to_reduced[full_idx] {
                    p[ri] += p_mw / network.base_mva;
                }
            }
        }
        p
    };

    let mut p = effective_p_inj.clone();
    if prepared.klu.solve(&mut p).is_err() {
        error!(buses = n, "DC B' solve failed");
        return Err(DcError::SingularMatrix);
    }

    let mut theta = reconstruct_theta(n, &prepared.bprime.full_to_reduced, &p);

    let mut branch_p_flow = compute_branch_flows_subset(
        network,
        theta.as_slice(),
        prepared.branch_indices,
        prepared.bus_map,
    );
    let slack_p_1 = compute_slack_injection_subset(
        network,
        &branch_p_flow,
        prepared.branch_indices,
        prepared.bus_map,
        slack_idx,
    );

    let (slack_p_injection, slack_distribution, total_generation_mw, p_inject_pu) =
        if let Some(pf_weights) = opts.participation_factors.as_ref() {
            if pf_weights.is_empty() {
                warn!("participation_factors list is empty — falling back to single slack");
                let p_inj_full = expand_p_inject(
                    n,
                    &prepared.bprime.full_to_reduced,
                    &effective_p_inj,
                    slack_idx,
                    slack_p_1,
                );
                (slack_p_1, HashMap::new(), 0.0, p_inj_full)
            } else {
                apply_participation_factor_slack(
                    network,
                    &mut prepared,
                    pf_weights,
                    &effective_p_inj,
                    slack_p_1,
                    slack_idx,
                    &mut theta,
                    &mut branch_p_flow,
                )?
            }
        } else if let Some(participating_buses) = opts.headroom_slack_bus_indices.as_ref() {
            if participating_buses.is_empty() {
                warn!("headroom slack bus list is empty — falling back to single slack");
                let p_inj_full = expand_p_inject(
                    n,
                    &prepared.bprime.full_to_reduced,
                    &effective_p_inj,
                    slack_idx,
                    slack_p_1,
                );
                (slack_p_1, HashMap::new(), 0.0, p_inj_full)
            } else {
                apply_headroom_slack(
                    network,
                    &mut prepared,
                    participating_buses,
                    &effective_p_inj,
                    slack_p_1,
                    slack_idx,
                    &mut theta,
                    &mut branch_p_flow,
                )?
            }
        } else {
            let p_inj_full = expand_p_inject(
                n,
                &prepared.bprime.full_to_reduced,
                &effective_p_inj,
                slack_idx,
                slack_p_1,
            );
            (slack_p_1, HashMap::new(), 0.0, p_inj_full)
        };

    let solve_time = start.elapsed().as_secs_f64();
    info!(
        buses = n,
        branches = prepared.branch_indices.len(),
        solve_time_ms = format_args!("{:.3}", solve_time * 1000.0),
        "DC power flow solved"
    );

    apply_angle_reference_subset(
        &mut theta,
        network,
        prepared.bus_indices,
        slack_idx,
        prepared.reference_angle0_rad,
        opts.angle_reference,
    );

    Ok(DcPfSolution {
        theta,
        branch_p_flow,
        slack_p_injection,
        solve_time_secs: solve_time,
        total_generation_mw,
        slack_distribution,
        p_inject_pu,
        island_ids: vec![],
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Apply headroom-limited slack balancing: redistribute the initial slack
/// mismatch across participating buses in proportion to generator headroom,
/// then re-solve the B' system for updated angles and flows.
///
/// Returns `(slack_p, slack_distribution, total_gen_mw, p_inject_pu)`.
#[allow(clippy::too_many_arguments)]
fn apply_headroom_slack(
    network: &Network,
    prepared: &mut PreparedSolveInputs<'_>,
    participating_buses: &[usize],
    effective_p_inj: &[f64],
    initial_slack_p: f64,
    slack_idx: usize,
    theta: &mut Vec<f64>,
    branch_p_flow: &mut Vec<f64>,
) -> Result<(f64, HashMap<usize, f64>, f64, Vec<f64>), DcError> {
    let n = network.n_buses();
    let mut p_adj = effective_p_inj.to_vec();
    let mut slack_dist: HashMap<usize, f64> = HashMap::with_capacity(participating_buses.len());

    let mut bus_headroom_pu: HashMap<usize, f64> = HashMap::new();
    for &bus_full_idx in participating_buses {
        if bus_full_idx != slack_idx
            && prepared
                .bprime
                .full_to_reduced
                .get(bus_full_idx)
                .copied()
                .flatten()
                .is_some()
        {
            bus_headroom_pu.entry(bus_full_idx).or_insert(0.0);
        }
    }
    for g in network.generators.iter().filter(|g| g.in_service) {
        if let Some(&bidx) = prepared.bus_map.get(&g.bus)
            && let Some(h) = bus_headroom_pu.get_mut(&bidx)
        {
            let headroom = if initial_slack_p >= 0.0 {
                (g.pmax - g.p) / network.base_mva
            } else {
                (g.p - g.pmin) / network.base_mva
            };
            *h += headroom.max(0.0);
        }
    }

    let total_headroom_pu: f64 = bus_headroom_pu.values().sum();

    if total_headroom_pu < 1e-12 {
        warn!(
            mismatch_mw = initial_slack_p.abs() * network.base_mva,
            "headroom slack: no participating generator has headroom \
             — mismatch absorbed by slack bus"
        );
    } else {
        let absorbable_pu = initial_slack_p.abs().min(total_headroom_pu);
        if initial_slack_p.abs() > total_headroom_pu + 1e-9 {
            warn!(
                total_headroom_mw = total_headroom_pu * network.base_mva,
                mismatch_mw = initial_slack_p.abs() * network.base_mva,
                "headroom slack: participant headroom exhausted \
                 — slack bus absorbs remainder"
            );
        }

        for (&bus_full_idx, &headroom) in &bus_headroom_pu {
            let share_pu =
                (headroom / total_headroom_pu) * absorbable_pu * initial_slack_p.signum();
            slack_dist.insert(bus_full_idx, share_pu * network.base_mva);
            if let Some(ri) = prepared.bprime.full_to_reduced[bus_full_idx] {
                p_adj[ri] += share_pu;
            }
        }
    }

    let p_eff_reduced = p_adj.clone();

    if prepared.klu.solve(&mut p_adj).is_err() {
        return Err(DcError::SingularMatrix);
    }

    *theta = reconstruct_theta(n, &prepared.bprime.full_to_reduced, &p_adj);
    *branch_p_flow = compute_branch_flows_subset(
        network,
        theta.as_slice(),
        prepared.branch_indices,
        prepared.bus_map,
    );
    let slack_p_2 = compute_slack_injection_subset(
        network,
        branch_p_flow,
        prepared.branch_indices,
        prepared.bus_map,
        slack_idx,
    );

    let total_load_mw = network.total_load_mw();
    let total_gen = total_load_mw + slack_p_2 * network.base_mva;

    debug!(
        participating_buses = participating_buses.len(),
        total_gen_mw = total_gen,
        remaining_slack_pu = slack_p_2,
        "headroom slack applied"
    );

    slack_dist.insert(slack_idx, slack_p_2 * network.base_mva);

    let p_inj_full = expand_p_inject(
        n,
        &prepared.bprime.full_to_reduced,
        &p_eff_reduced,
        slack_idx,
        slack_p_2,
    );
    Ok((slack_p_2, slack_dist, total_gen, p_inj_full))
}

/// Apply participation-factor distributed slack: redistribute the initial slack
/// mismatch across participating buses in proportion to their weights, then
/// re-solve the B' system for updated angles and flows.
///
/// Returns `(slack_p, slack_distribution, total_gen_mw, p_inject_pu)`.
#[allow(clippy::too_many_arguments)]
fn apply_participation_factor_slack(
    network: &Network,
    prepared: &mut PreparedSolveInputs<'_>,
    weights: &[(usize, f64)],
    effective_p_inj: &[f64],
    initial_slack_p: f64,
    slack_idx: usize,
    theta: &mut Vec<f64>,
    branch_p_flow: &mut Vec<f64>,
) -> Result<(f64, HashMap<usize, f64>, f64, Vec<f64>), DcError> {
    let n = network.n_buses();
    let mut p_adj = effective_p_inj.to_vec();
    let mut slack_dist: HashMap<usize, f64> = HashMap::with_capacity(weights.len());

    // Normalize weights to sum to 1.0, filtering to valid reduced-bus entries.
    let mut valid_weights: Vec<(usize, f64)> = Vec::with_capacity(weights.len());
    for &(bus_idx, w) in weights {
        if w > 0.0
            && w.is_finite()
            && bus_idx != slack_idx
            && bus_idx < n
            && prepared
                .bprime
                .full_to_reduced
                .get(bus_idx)
                .copied()
                .flatten()
                .is_some()
        {
            valid_weights.push((bus_idx, w));
        }
    }
    let total_weight: f64 = valid_weights.iter().map(|(_, w)| w).sum();
    if total_weight < 1e-12 {
        warn!("participation factor slack: no valid weights — mismatch absorbed by slack bus");
        let p_inj_full = expand_p_inject(
            n,
            &prepared.bprime.full_to_reduced,
            effective_p_inj,
            slack_idx,
            initial_slack_p,
        );
        return Ok((initial_slack_p, HashMap::new(), 0.0, p_inj_full));
    }

    // Distribute the full mismatch proportionally. Unlike headroom slack,
    // participation-factor slack has no capacity limit — all mismatch is absorbed.
    for &(bus_idx, w) in &valid_weights {
        let share_pu = (w / total_weight) * initial_slack_p;
        slack_dist.insert(bus_idx, share_pu * network.base_mva);
        if let Some(ri) = prepared.bprime.full_to_reduced[bus_idx] {
            p_adj[ri] += share_pu;
        }
    }

    let p_eff_reduced = p_adj.clone();

    if prepared.klu.solve(&mut p_adj).is_err() {
        return Err(DcError::SingularMatrix);
    }

    *theta = reconstruct_theta(n, &prepared.bprime.full_to_reduced, &p_adj);
    *branch_p_flow = compute_branch_flows_subset(
        network,
        theta.as_slice(),
        prepared.branch_indices,
        prepared.bus_map,
    );
    let slack_p_2 = compute_slack_injection_subset(
        network,
        branch_p_flow,
        prepared.branch_indices,
        prepared.bus_map,
        slack_idx,
    );

    let total_load_mw = network.total_load_mw();
    let total_gen = total_load_mw + slack_p_2 * network.base_mva;

    debug!(
        n_participants = valid_weights.len(),
        total_gen_mw = total_gen,
        remaining_slack_pu = slack_p_2,
        "participation factor slack applied"
    );

    slack_dist.insert(slack_idx, slack_p_2 * network.base_mva);

    let p_inj_full = expand_p_inject(
        n,
        &prepared.bprime.full_to_reduced,
        &p_eff_reduced,
        slack_idx,
        slack_p_2,
    );
    Ok((slack_p_2, slack_dist, total_gen, p_inj_full))
}

/// Expand a reduced (slack-removed) injection vector to a full-bus vector,
/// inserting the back-calculated slack injection at `slack_idx`.
fn expand_p_inject(
    n: usize,
    full_to_reduced: &[Option<usize>],
    p_reduced: &[f64],
    slack_idx: usize,
    slack_p: f64,
) -> Vec<f64> {
    let mut p_full = vec![0.0f64; n];
    for (full_idx, ri) in full_to_reduced.iter().enumerate() {
        if let Some(ri) = ri {
            p_full[full_idx] = p_reduced[*ri];
        }
    }
    p_full[slack_idx] = slack_p;
    p_full
}

/// Reconstruct the full-bus theta vector from the reduced (slack-removed) solution.
fn reconstruct_theta(
    n: usize,
    full_to_reduced: &[Option<usize>],
    theta_reduced: &[f64],
) -> Vec<f64> {
    let mut theta = vec![0.0; n];
    for (full_idx, ri) in full_to_reduced.iter().enumerate() {
        if let Some(ri) = ri {
            theta[full_idx] = theta_reduced[*ri];
        }
    }
    theta
}

fn compute_branch_flows_subset(
    network: &Network,
    theta: &[f64],
    branch_indices: &[usize],
    bus_map: &HashMap<u32, usize>,
) -> Vec<f64> {
    let mut branch_p_flow = vec![0.0; network.n_branches()];
    for &br_idx in branch_indices {
        let branch = &network.branches[br_idx];
        let from_idx = bus_map[&branch.from_bus];
        let to_idx = bus_map[&branch.to_bus];
        let phi_rad = branch.phase_shift_rad;
        branch_p_flow[br_idx] = branch.b_dc() * (theta[from_idx] - theta[to_idx] - phi_rad);
    }
    branch_p_flow
}

fn compute_slack_injection_subset(
    network: &Network,
    branch_p_flow: &[f64],
    branch_indices: &[usize],
    bus_map: &HashMap<u32, usize>,
    slack_idx: usize,
) -> f64 {
    let mut slack_p = 0.0;
    for &br_idx in branch_indices {
        let branch = &network.branches[br_idx];
        let from_idx = bus_map[&branch.from_bus];
        let to_idx = bus_map[&branch.to_bus];
        if from_idx == slack_idx {
            slack_p += branch_p_flow[br_idx];
        }
        if to_idx == slack_idx {
            slack_p -= branch_p_flow[br_idx];
        }
    }
    slack_p
}

/// Convert a DC power flow result into the common [`PfSolution`] format.
///
/// # Hardcoded DC Power Flow Values
///
/// The DC power flow model does not compute voltage magnitudes or reactive power
/// (see the [`crate`]-level documentation for the three DC PF assumptions). The
/// following fields in the returned [`PfSolution`] are therefore set to
/// placeholder values reflecting the DC assumptions:
///
/// - **`vm`** (voltage magnitudes) — hardcoded to **1.0 p.u.** for all buses.
///   The DC power flow assumption `|V_i| = 1.0` means voltage magnitudes are not
///   solved for. Users requiring actual voltage magnitudes (e.g., for voltage
///   violation checking, tap optimization, or reactive planning) must use the
///   AC Newton-Raphson solver in `surge_ac`.
///
/// - **`q_inject`** (reactive power injections) — hardcoded to **0.0** for all buses.
///   The DC power flow decouples P from Q by assuming flat voltages and lossless
///   branches. Reactive power flow, generator Q limits, and shunt compensation
///   effects are not captured. Use AC power flow for reactive power analysis.
///
/// - **`q_limited_buses`** — empty (no Q-limit enforcement in DC PF).
///
/// - **`iterations`** — always 1 (DC PF is a single sparse linear solve, not iterative).
///
/// - **`max_mismatch`** — always 0.0 (the linear system is solved exactly).
///
/// The `va` (voltage angles) and `p_inject` (real power injections) fields contain
/// the actual DC power flow results and are physically meaningful within the DC
/// approximation.
pub fn to_pf_solution(result: &DcPfSolution, network: &Network) -> PfSolution {
    let n = network.n_buses();

    // Use the effective injection vector stored in the result — this includes
    // all RHS correction terms (Gs shunts, PST phantom injections, HVDC
    // schedules, MTDC injections, distributed-slack shares) and is
    // self-consistent with branch_p_flow by construction.
    let p_inject = result.p_inject_pu.clone();
    let base = network.base_mva;
    let branch_p_from_mw: Vec<f64> = result.branch_p_flow.iter().map(|&f| f * base).collect();
    let branch_p_to_mw: Vec<f64> = branch_p_from_mw.iter().map(|&f| -f).collect();
    let n_branches = network.n_branches();

    PfSolution {
        pf_model: PfModel::Dc,
        status: SolveStatus::Converged,
        iterations: 1,     // DC PF is a single linear solve — not iterative
        max_mismatch: 0.0, // linear system solved exactly (no mismatch)
        solve_time_secs: result.solve_time_secs,
        // DC power flow assumes |V| = 1.0 p.u. for all buses — voltage magnitudes
        // are not computed. For actual Vm values, use AC power flow (surge_ac).
        voltage_magnitude_pu: vec![1.0; n],
        voltage_angle_rad: result.theta.clone(),
        active_power_injection_pu: p_inject,
        // DC power flow does not compute reactive power — Q is decoupled from P
        // under the flat-voltage, lossless-branch assumptions. For reactive power
        // analysis, use AC power flow (surge_ac).
        reactive_power_injection_pu: vec![0.0; n],
        branch_p_from_mw,
        branch_p_to_mw,
        branch_q_from_mvar: vec![0.0; n_branches],
        branch_q_to_mvar: vec![0.0; n_branches],
        bus_numbers: network.buses.iter().map(|b| b.number).collect(),
        island_ids: result.island_ids.clone(),
        q_limited_buses: vec![], // no Q-limit enforcement in DC PF
        n_q_limit_switches: 0,
        gen_slack_contribution_mw: vec![],
        convergence_history: vec![],
        worst_mismatch_bus: None,
        area_interchange: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};
    use surge_network::{AngleReference, DistributedAngleWeight};

    fn check_kcl(net: &Network, result: &DcPfSolution, tol: f64) {
        let bus_map = net.bus_index_map();
        let p_inj = net.bus_p_injection_pu();
        let slack_idx = net.slack_bus_index().unwrap();
        let mut bus_flow_sum = vec![0.0; net.n_buses()];

        for (br_idx, branch) in net.branches.iter().enumerate() {
            if !branch.in_service {
                continue;
            }
            let from = bus_map[&branch.from_bus];
            let to = bus_map[&branch.to_bus];
            bus_flow_sum[from] += result.branch_p_flow[br_idx];
            bus_flow_sum[to] -= result.branch_p_flow[br_idx];
        }

        for i in 0..net.n_buses() {
            if i == slack_idx {
                continue;
            }
            assert!(
                (bus_flow_sum[i] - p_inj[i]).abs() < tol,
                "KCL violated at bus {} (idx {}): flow_sum={:.6e}, p_inj={:.6e}",
                net.buses[i].number,
                i,
                bus_flow_sum[i],
                p_inj[i]
            );
        }
    }

    #[test]
    fn test_dc_case9() {
        skip_if_no_data!();
        let net = load_net("case9");
        let result = solve_dc(&net).expect("DC PF should converge");

        assert_eq!(result.theta.len(), 9);
        assert_eq!(result.theta[net.slack_bus_index().unwrap()], 0.0);
        for &angle in &result.theta {
            assert!(angle.abs() < 1.0, "unreasonable angle: {angle} rad");
        }
        check_kcl(&net, &result, 1e-8);
    }

    #[test]
    fn test_dc_case14() {
        skip_if_no_data!();
        let net = load_net("case14");
        let result = solve_dc(&net).expect("DC PF should converge");

        assert_eq!(result.theta.len(), 14);
        assert_eq!(result.theta[net.slack_bus_index().unwrap()], 0.0);
        check_kcl(&net, &result, 1e-8);
    }

    #[test]
    fn test_dc_case118() {
        skip_if_no_data!();
        let net = load_net("case118");
        let result = solve_dc(&net).expect("DC PF should converge");

        assert_eq!(result.theta.len(), 118);
        check_kcl(&net, &result, 1e-6);
    }

    #[test]
    fn test_dc_case2383wp() {
        skip_if_no_data!();
        let net = load_net("case2383wp");
        let result = solve_dc(&net).expect("DC PF should converge on case2383wp");

        assert_eq!(result.theta.len(), 2383);
        assert_eq!(result.theta[net.slack_bus_index().unwrap()], 0.0);
        check_kcl(&net, &result, 1e-5);
    }

    /// Multi-island test: case16ci has 3 slack buses (3 islands).
    #[test]
    fn test_dc_multi_island_case16ci() {
        skip_if_no_data!();
        let net = load_net("case16ci");
        let result = solve_dc(&net).expect("DC PF should converge on multi-island case16ci");
        assert_eq!(result.theta.len(), 16);
        // All angles should be reasonable (< 1 radian)
        for &angle in &result.theta {
            assert!(angle.abs() < 1.0, "unreasonable angle: {angle} rad");
        }
    }

    #[test]
    fn test_dc_solver_rejects_missing_slack_bus() {
        let mut net = Network::new("dc_missing_slack");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::PQ, 230.0));
        net.buses.push(Bus::new(2, BusType::PQ, 230.0));
        net.branches.push(Branch::new_line(1, 2, 0.0, 0.1, 0.0));
        net.loads.push(Load::new(2, 50.0, 0.0));

        let err = solve_dc_opts(&net, &DcPfOptions::default()).unwrap_err();
        match err {
            DcError::InvalidNetwork(msg) => {
                assert!(
                    msg.contains("slack"),
                    "validation error should mention slack placement, got: {msg}"
                );
            }
            other => panic!("expected invalid-network error from DC validation, got {other:?}"),
        }
    }

    #[test]
    fn test_multi_island_dc_line_injections_are_preserved() {
        use surge_network::network::{
            Branch, Bus, Generator, LccConverterTerminal, LccHvdcLink, Load,
        };

        let mut net = Network::new("multi_island_dc_line");
        net.base_mva = 100.0;

        let mut bus1 = Bus::new(1, BusType::Slack, 230.0);
        bus1.voltage_angle_rad = 0.05;
        net.buses.push(bus1);

        let bus2 = Bus::new(2, BusType::PQ, 230.0);
        net.buses.push(bus2);
        net.loads.push(Load::new(2, 100.0, 0.0));

        net.buses.push(Bus::new(3, BusType::Slack, 230.0));
        net.buses.push(Bus::new(4, BusType::PQ, 230.0));
        net.generators.push(Generator::new(1, 100.0, 1.0));
        net.generators.push(Generator::new(3, 0.0, 1.0));

        net.branches.push(Branch::new_line(1, 2, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(3, 4, 0.0, 0.1, 0.0));

        let base = solve_dc(&net).expect("base multi-island DC solve");

        let dc_line = LccHvdcLink {
            name: "L12".to_string(),
            scheduled_setpoint: 40.0,
            scheduled_voltage_kv: 500.0,
            rectifier: LccConverterTerminal {
                bus: 2,
                in_service: true,
                ..LccConverterTerminal::default()
            },
            inverter: LccConverterTerminal {
                bus: 4,
                in_service: true,
                ..LccConverterTerminal::default()
            },
            ..LccHvdcLink::default()
        };
        net.hvdc.push_lcc_link(dc_line);

        let with_dc = solve_dc(&net).expect("multi-island DC solve with DC line");

        assert!(
            (with_dc.branch_p_flow[0] - base.branch_p_flow[0] - 0.4).abs() < 1e-9,
            "island-A flow should include the rectifier withdrawal"
        );
        assert!(
            (with_dc.branch_p_flow[1] - base.branch_p_flow[1] + 0.4).abs() < 1e-9,
            "island-B flow should include the inverter injection"
        );
        assert!(
            (with_dc.p_inject_pu[1] + 1.4).abs() < 1e-9,
            "bus 2 should include the extra DC-line withdrawal"
        );
        assert!(
            (with_dc.p_inject_pu[3] - 0.4).abs() < 1e-9,
            "bus 4 should include the DC-line injection"
        );
    }

    fn build_two_island_prepared_test_network() -> Network {
        use surge_network::network::{Branch, Bus, Generator, Load};

        let mut net = Network::new("two_island_prepared_dc");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PQ, 230.0));
        net.loads.push(Load::new(2, 50.0, 0.0));
        net.buses.push(Bus::new(3, BusType::PQ, 230.0));
        net.loads.push(Load::new(3, 50.0, 0.0));

        net.buses.push(Bus::new(4, BusType::Slack, 230.0));
        net.buses.push(Bus::new(5, BusType::PQ, 230.0));
        net.loads.push(Load::new(5, 50.0, 0.0));
        net.buses.push(Bus::new(6, BusType::PQ, 230.0));
        net.loads.push(Load::new(6, 50.0, 0.0));

        net.branches.push(Branch::new_line(1, 2, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(2, 3, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(1, 3, 0.0, 0.2, 0.0));

        net.branches.push(Branch::new_line(4, 5, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(5, 6, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(4, 6, 0.0, 0.2, 0.0));

        net.generators.push(Generator::new(1, 100.0, 1.0));
        net.generators.push(Generator::new(4, 100.0, 1.0));

        net
    }

    fn build_two_island_headroom_test_network() -> Network {
        use surge_network::network::{Branch, Bus, Generator, Load};

        let mut net = Network::new("two_island_headroom_dc");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PV, 230.0));
        net.buses.push(Bus::new(3, BusType::PQ, 230.0));
        net.loads.push(Load::new(3, 90.0, 0.0));

        net.buses.push(Bus::new(4, BusType::Slack, 230.0));
        net.buses.push(Bus::new(5, BusType::PV, 230.0));
        net.buses.push(Bus::new(6, BusType::PQ, 230.0));
        net.loads.push(Load::new(6, 80.0, 0.0));

        net.branches.push(Branch::new_line(1, 2, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(2, 3, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(1, 3, 0.0, 0.2, 0.0));
        net.branches.push(Branch::new_line(4, 5, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(5, 6, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(4, 6, 0.0, 0.2, 0.0));

        let mut g1 = Generator::new(1, 25.0, 1.0);
        g1.pmax = 120.0;
        net.generators.push(g1);

        let mut g2 = Generator::new(2, 25.0, 1.0);
        g2.pmax = 90.0;
        net.generators.push(g2);

        let mut g3 = Generator::new(4, 20.0, 1.0);
        g3.pmax = 110.0;
        net.generators.push(g3);

        let mut g4 = Generator::new(5, 20.0, 1.0);
        g4.pmax = 85.0;
        net.generators.push(g4);

        net
    }

    #[test]
    fn test_prepared_model_supports_multi_island_networks() {
        let net = build_two_island_prepared_test_network();
        let mut model = PreparedDcStudy::new(&net).expect("prepared multi-island model");

        let prepared = model
            .solve(&DcPfOptions::default())
            .expect("prepared solve");
        let wrapped = solve_dc(&net).expect("wrapped solve");
        assert_eq!(prepared.theta, wrapped.theta);
        assert_eq!(prepared.branch_p_flow, wrapped.branch_p_flow);
        assert_eq!(prepared.p_inject_pu, wrapped.p_inject_pu);

        let monitored = vec![0usize, 3usize];
        let ptdf = model
            .compute_ptdf_request(
                &crate::sensitivity::PtdfRequest::for_branches(&monitored)
                    .with_bus_indices(&[0, 1, 2, 3, 4, 5]),
            )
            .expect("prepared ptdf");
        let wrapped_ptdf = crate::sensitivity::compute_ptdf(
            &net,
            &crate::sensitivity::PtdfRequest::for_branches(&monitored)
                .with_bus_indices(&[0, 1, 2, 3, 4, 5]),
        )
        .expect("wrapped ptdf");
        assert_eq!(ptdf, wrapped_ptdf);
        let row0 = ptdf.row(0).expect("row 0");
        let row3 = ptdf.row(3).expect("row 3");
        assert!(row0[3].abs() < 1e-12);
        assert!(row0[4].abs() < 1e-12);
        assert!(row0[5].abs() < 1e-12);
        assert!(row3[0].abs() < 1e-12);
        assert!(row3[1].abs() < 1e-12);
        assert!(row3[2].abs() < 1e-12);

        let lodf = model
            .compute_lodf(&monitored, &monitored)
            .expect("prepared lodf");
        let wrapped_lodf = crate::sensitivity::compute_lodf(
            &net,
            &crate::sensitivity::LodfRequest::for_branches(&monitored, &monitored),
        )
        .expect("wrapped lodf");
        assert_eq!(lodf, wrapped_lodf);
        assert!(lodf[(0, 1)].abs() < 1e-12);
        assert!(lodf[(1, 0)].abs() < 1e-12);

        let n2 = model
            .compute_n2_lodf((0, 3), &monitored)
            .expect("prepared n2");
        let wrapped_n2 = crate::sensitivity::compute_n2_lodf(
            &net,
            &crate::sensitivity::N2LodfRequest::new((0, 3)).with_monitored_branches(&monitored),
        )
        .expect("wrapped n2");
        assert_eq!(n2, wrapped_n2);
    }

    #[test]
    fn test_multi_island_headroom_metadata_matches_prepared_study() {
        let net = build_two_island_headroom_test_network();
        let opts = DcPfOptions::with_headroom_slack(&[0, 1, 3, 4]);

        let one_shot = solve_dc_opts(&net, &opts).expect("one-shot multi-island solve");
        let mut study = PreparedDcStudy::new(&net).expect("prepared multi-island study");
        let prepared = study.solve(&opts).expect("prepared multi-island solve");

        assert_eq!(one_shot.theta, prepared.theta);
        assert_eq!(one_shot.branch_p_flow, prepared.branch_p_flow);
        assert_eq!(one_shot.p_inject_pu, prepared.p_inject_pu);
        assert!((one_shot.slack_p_injection - prepared.slack_p_injection).abs() < 1e-12);
        assert!((one_shot.total_generation_mw - prepared.total_generation_mw).abs() < 1e-9);
        assert_eq!(one_shot.slack_distribution, prepared.slack_distribution);
    }

    #[test]
    fn test_unchecked_singleton_island_keeps_island_id_and_slack_accounting() {
        let mut net = Network::new("singleton_island");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PQ, 230.0));
        net.buses.push(Bus::new(3, BusType::PQ, 230.0));
        net.branches.push(Branch::new_line(1, 2, 0.0, 0.1, 0.0));
        net.loads.push(Load::new(2, 40.0, 0.0));
        net.loads.push(Load::new(3, 15.0, 0.0));

        let mut g1 = Generator::new(1, 40.0, 1.0);
        g1.pmax = 100.0;
        net.generators.push(g1);

        let mut study = PreparedDcStudy::new_unchecked(&net).expect("prepared study");
        let result = study.solve(&DcPfOptions::default()).expect("dc solve");

        assert_eq!(result.island_ids, vec![0, 0, 1]);
        assert!(
            (result.slack_p_injection - 0.55).abs() < 1e-12,
            "total slack accounting should include both the main island and the singleton island, got {:.6}",
            result.slack_p_injection
        );
        assert!(
            (result.p_inject_pu[2] - 0.15).abs() < 1e-12,
            "singleton island bus should keep its local slack injection, got {:.6}",
            result.p_inject_pu[2]
        );
    }

    #[test]
    fn test_singleton_island_applies_external_injections() {
        let mut net = Network::new("singleton_external_injection");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.loads.push(Load::new(1, 10.0, 0.0));

        let mut study = PreparedDcStudy::new_unchecked(&net).expect("prepared study");
        let opts = DcPfOptions {
            external_p_injections_mw: vec![(1, 5.0)],
            ..DcPfOptions::default()
        };
        let result = study.solve(&opts).expect("dc solve");

        assert!(
            (result.p_inject_pu[0] - 0.05).abs() < 1e-12,
            "singleton island should include external injections in slack accounting, got {:.6}",
            result.p_inject_pu[0]
        );
    }

    // -----------------------------------------------------------------------
    // PST-01: Phase-shifting transformer tests
    // -----------------------------------------------------------------------

    /// Build a 3-bus network with a PST on branch bus1->bus2.
    ///
    /// Bus 1 (idx 0): slack (angle = 0)
    /// Bus 2 (idx 1): PQ load 100 MW
    /// Bus 3 (idx 2): PV generator 100 MW
    /// Branch 0 (bus1->bus2): x=0.1, shift=phi_deg converted to radians (PST)
    /// Branch 1 (bus2->bus3): x=0.1, no shift
    /// Branch 2 (bus1->bus3): x=0.2, no shift
    fn build_3bus_pst_network(phi_deg: f64) -> Network {
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};

        let mut net = Network::new("3bus_pst");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 345.0));
        net.generators.push(Generator::new(1, 0.0, 1.0));

        net.buses.push(Bus::new(2, BusType::PQ, 345.0));
        net.loads.push(Load::new(2, 100.0, 0.0));

        net.buses.push(Bus::new(3, BusType::PV, 345.0));

        net.generators.push(Generator::new(3, 100.0, 1.0));

        // PST branch (bus 1 -> bus 2 with phase shift)
        let mut br_pst = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br_pst.phase_shift_rad = phi_deg.to_radians();
        net.branches.push(br_pst);

        // Normal lines
        net.branches.push(Branch::new_line(2, 3, 0.0, 0.1, 0.0));
        net.branches.push(Branch::new_line(1, 3, 0.0, 0.2, 0.0));

        net
    }

    /// PST-01: DC flows must change when a non-zero phase shift is applied.
    #[test]
    fn test_pst_dc_flow_changes_with_shift() {
        skip_if_no_data!();
        let net_no_shift = build_3bus_pst_network(0.0);
        let net_with_shift = build_3bus_pst_network(5.0);

        let r0 = solve_dc(&net_no_shift).expect("DC PF should converge");
        let r5 = solve_dc(&net_with_shift).expect("DC PF should converge");

        // At least one branch flow must differ.
        let all_same = r0
            .branch_p_flow
            .iter()
            .zip(r5.branch_p_flow.iter())
            .all(|(a, b)| (a - b).abs() < 1e-10);

        assert!(
            !all_same,
            "DC flows should change when PST shift is non-zero; \
             no-shift flows: {:?}, with-shift flows: {:?}",
            r0.branch_p_flow, r5.branch_p_flow
        );

        // The PST branch itself must see a significant flow change.
        let delta = (r0.branch_p_flow[0] - r5.branch_p_flow[0]).abs();
        assert!(
            delta > 1e-4,
            "PST branch flow should change noticeably: no-shift={:.4}, with-shift={:.4}",
            r0.branch_p_flow[0],
            r5.branch_p_flow[0]
        );
    }

    /// PST-01: KCL must hold on the PST network.
    #[test]
    fn test_pst_kcl_satisfied() {
        skip_if_no_data!();
        let net = build_3bus_pst_network(10.0);
        let result = solve_dc(&net).expect("DC PF should converge");
        check_kcl(&net, &result, 1e-8);
    }

    /// PST-01: case9 (no PSTs) gives identical KCL results with the updated solver.
    #[test]
    fn test_no_pst_case9_unchanged() {
        skip_if_no_data!();
        let net = load_net("case9");
        for br in &net.branches {
            assert!(
                br.phase_shift_rad.abs() < 1e-12,
                "case9 branch {}->{}  has unexpected shift={}",
                br.from_bus,
                br.to_bus,
                br.phase_shift_rad
            );
        }
        let result = solve_dc(&net).expect("DC PF should converge");
        check_kcl(&net, &result, 1e-8);
        assert_eq!(result.theta[net.slack_bus_index().unwrap()], 0.0);
    }

    /// PST-01: case14 (no PSTs) gives identical KCL results with the updated solver.
    #[test]
    fn test_no_pst_case14_unchanged() {
        skip_if_no_data!();
        let net = load_net("case14");
        for br in &net.branches {
            assert!(
                br.phase_shift_rad.abs() < 1e-12,
                "case14 branch {}->{} has unexpected shift={}",
                br.from_bus,
                br.to_bus,
                br.phase_shift_rad
            );
        }
        let result = solve_dc(&net).expect("DC PF should converge");
        check_kcl(&net, &result, 1e-8);
    }

    // -----------------------------------------------------------------------
    // PST-02: Distributed slack tests
    // -----------------------------------------------------------------------

    /// PST-02: `solve_dc_opts` with default options produces the same result as `solve_dc`.
    #[test]
    fn test_default_opts_identical_to_solve_dc() {
        skip_if_no_data!();
        let net = load_net("case9");
        let r1 = solve_dc(&net).expect("solve_dc");
        let r2 = solve_dc_opts(&net, &DcPfOptions::default()).expect("solve_dc_opts default");

        assert_eq!(r1.theta.len(), r2.theta.len());
        for (a, b) in r1.theta.iter().zip(r2.theta.iter()) {
            assert!((a - b).abs() < 1e-12, "theta differs: {a:.8e} vs {b:.8e}");
        }
        for (a, b) in r1.branch_p_flow.iter().zip(r2.branch_p_flow.iter()) {
            assert!((a - b).abs() < 1e-12, "flow differs: {a:.8e} vs {b:.8e}");
        }
    }

    #[test]
    fn test_angle_reference_changes_only_reported_theta() {
        let mut net = build_3bus_pst_network(5.0);
        net.buses[0].voltage_angle_rad = 0.12;

        let preserve = solve_dc_opts(&net, &DcPfOptions::default()).expect("preserve solve");
        let zero = solve_dc_opts(
            &net,
            &DcPfOptions::default().with_angle_reference(AngleReference::Zero),
        )
        .expect("zero-reference solve");
        let distributed = solve_dc_opts(
            &net,
            &DcPfOptions::default().with_angle_reference(AngleReference::Distributed(
                DistributedAngleWeight::LoadWeighted,
            )),
        )
        .expect("distributed-reference solve");

        assert!((preserve.theta[0] - 0.12).abs() < 1e-12);
        assert!(zero.theta[0].abs() < 1e-12);

        let bus2_idx = net.bus_index_map()[&2];
        assert!(distributed.theta[bus2_idx].abs() < 1e-12);

        assert!(
            preserve
                .theta
                .iter()
                .zip(zero.theta.iter())
                .any(|(a, b)| (a - b).abs() > 1e-9),
            "angle reference should change the reported theta vector"
        );

        for ((flow_p, flow_z), flow_d) in preserve
            .branch_p_flow
            .iter()
            .zip(zero.branch_p_flow.iter())
            .zip(distributed.branch_p_flow.iter())
        {
            assert!((flow_p - flow_z).abs() < 1e-12);
            assert!((flow_p - flow_d).abs() < 1e-12);
        }
        for ((inj_p, inj_z), inj_d) in preserve
            .p_inject_pu
            .iter()
            .zip(zero.p_inject_pu.iter())
            .zip(distributed.p_inject_pu.iter())
        {
            assert!((inj_p - inj_z).abs() < 1e-12);
            assert!((inj_p - inj_d).abs() < 1e-12);
        }
        assert!((preserve.slack_p_injection - zero.slack_p_injection).abs() < 1e-12);
        assert!((preserve.slack_p_injection - distributed.slack_p_injection).abs() < 1e-12);
    }

    #[test]
    fn test_multi_island_angle_reference_is_applied_per_island() {
        let mut net = build_two_island_prepared_test_network();
        net.buses[0].voltage_angle_rad = 0.15;
        net.buses[3].voltage_angle_rad = -0.25;

        let preserve = solve_dc_opts(&net, &DcPfOptions::default()).expect("preserve solve");
        let zero = solve_dc_opts(
            &net,
            &DcPfOptions::default().with_angle_reference(AngleReference::Zero),
        )
        .expect("zero-reference solve");
        let distributed = solve_dc_opts(
            &net,
            &DcPfOptions::default().with_angle_reference(AngleReference::Distributed(
                DistributedAngleWeight::LoadWeighted,
            )),
        )
        .expect("distributed-reference solve");

        assert!((preserve.theta[0] - 0.15).abs() < 1e-12);
        assert!((preserve.theta[3] + 0.25).abs() < 1e-12);
        assert!(zero.theta[0].abs() < 1e-12);
        assert!(zero.theta[3].abs() < 1e-12);

        let island_a_load_center = 0.5 * distributed.theta[1] + 0.5 * distributed.theta[2];
        let island_b_load_center = 0.5 * distributed.theta[4] + 0.5 * distributed.theta[5];
        assert!(island_a_load_center.abs() < 1e-12);
        assert!(island_b_load_center.abs() < 1e-12);

        for (flow_p, flow_d) in preserve
            .branch_p_flow
            .iter()
            .zip(distributed.branch_p_flow.iter())
        {
            assert!((flow_p - flow_d).abs() < 1e-12);
        }
    }

    #[test]
    fn test_angle_reference_remains_orthogonal_to_headroom_slack() {
        let mut net = build_two_island_headroom_test_network();
        net.buses[0].voltage_angle_rad = 0.08;
        net.buses[3].voltage_angle_rad = -0.11;

        let preserve_opts = DcPfOptions::with_headroom_slack(&[0, 1, 3, 4])
            .with_angle_reference(AngleReference::PreserveInitial);
        let zero_opts = DcPfOptions::with_headroom_slack(&[0, 1, 3, 4])
            .with_angle_reference(AngleReference::Zero);
        let distributed_opts = DcPfOptions::with_headroom_slack(&[0, 1, 3, 4])
            .with_angle_reference(AngleReference::Distributed(
                DistributedAngleWeight::LoadWeighted,
            ));

        let preserve = solve_dc_opts(&net, &preserve_opts).expect("preserve solve");
        let zero = solve_dc_opts(&net, &zero_opts).expect("zero-reference solve");
        let distributed =
            solve_dc_opts(&net, &distributed_opts).expect("distributed-reference solve");

        for ((flow_p, flow_z), flow_d) in preserve
            .branch_p_flow
            .iter()
            .zip(zero.branch_p_flow.iter())
            .zip(distributed.branch_p_flow.iter())
        {
            assert!((flow_p - flow_z).abs() < 1e-12);
            assert!((flow_p - flow_d).abs() < 1e-12);
        }

        assert_eq!(
            preserve.slack_distribution.len(),
            zero.slack_distribution.len()
        );
        assert_eq!(
            preserve.slack_distribution.len(),
            distributed.slack_distribution.len()
        );
        for (bus_idx, preserve_share) in &preserve.slack_distribution {
            let zero_share = zero
                .slack_distribution
                .get(bus_idx)
                .copied()
                .expect("zero-reference slack distribution key");
            let distributed_share = distributed
                .slack_distribution
                .get(bus_idx)
                .copied()
                .expect("distributed-reference slack distribution key");
            assert!((preserve_share - zero_share).abs() < 1e-12);
            assert!((preserve_share - distributed_share).abs() < 1e-12);
        }
    }

    #[test]
    fn test_prepared_model_matches_wrapper_apis() {
        skip_if_no_data!();

        let net = load_net("case14");
        let mut model = PreparedDcStudy::new(&net).expect("prepared model");

        let prepared = model
            .solve(&DcPfOptions::default())
            .expect("prepared solve");
        let wrapped = solve_dc(&net).expect("wrapper solve");
        assert_eq!(prepared.theta, wrapped.theta);
        assert_eq!(prepared.branch_p_flow, wrapped.branch_p_flow);
        assert_eq!(prepared.p_inject_pu, wrapped.p_inject_pu);

        let all_branches: Vec<usize> = (0..net.n_branches()).collect();
        let prepared_ptdf_rows = model
            .compute_ptdf(&all_branches)
            .expect("prepared ptdf rows");
        let prepared_ptdf = model.compute_ptdf(&all_branches).expect("prepared ptdf");
        let wrapped_ptdf = crate::sensitivity::compute_ptdf(
            &net,
            &crate::sensitivity::PtdfRequest::for_branches(&all_branches),
        )
        .expect("wrapper ptdf");
        assert_eq!(
            prepared_ptdf_rows.monitored_branches(),
            all_branches.as_slice()
        );
        assert_eq!(prepared_ptdf, wrapped_ptdf);

        let prepared_lodf = model
            .compute_lodf_matrix(&all_branches)
            .expect("prepared lodf");
        let wrapped_lodf = crate::sensitivity::compute_lodf_matrix(
            &net,
            &crate::sensitivity::LodfMatrixRequest::for_branches(&all_branches),
        )
        .expect("wrapper lodf");
        assert_eq!(prepared_lodf.n_rows(), wrapped_lodf.n_rows());
        assert_eq!(prepared_lodf.n_cols(), wrapped_lodf.n_cols());
        for i in 0..prepared_lodf.n_rows() {
            for j in 0..prepared_lodf.n_cols() {
                assert!(
                    (prepared_lodf[(i, j)] - wrapped_lodf[(i, j)]).abs() < 1e-12
                        || (prepared_lodf[(i, j)].is_infinite()
                            && wrapped_lodf[(i, j)].is_infinite())
                );
            }
        }

        let monitored = vec![0, 2, 5];
        let outages = vec![1, 6];
        let prepared_subset = model
            .compute_lodf(&monitored, &outages)
            .expect("prepared subset lodf");
        let wrapped_subset = crate::sensitivity::compute_lodf(
            &net,
            &crate::sensitivity::LodfRequest::for_branches(&monitored, &outages),
        )
        .expect("wrapper subset lodf");
        let prepared_otdf = model
            .compute_otdf(&monitored, &outages)
            .expect("prepared subset otdf");
        let wrapped_otdf = crate::sensitivity::compute_otdf(
            &net,
            &crate::sensitivity::OtdfRequest::new(&monitored, &outages),
        )
        .expect("wrapper subset otdf");
        assert_eq!(prepared_subset.n_rows(), monitored.len());
        assert_eq!(prepared_subset.n_cols(), outages.len());
        assert_eq!(prepared_subset.n_rows(), wrapped_subset.n_rows());
        assert_eq!(prepared_subset.n_cols(), wrapped_subset.n_cols());
        for i in 0..prepared_subset.n_rows() {
            for j in 0..prepared_subset.n_cols() {
                assert!(
                    (prepared_subset[(i, j)] - wrapped_subset[(i, j)]).abs() < 1e-12
                        || (prepared_subset[(i, j)].is_infinite()
                            && wrapped_subset[(i, j)].is_infinite())
                );
            }
        }
        assert_eq!(prepared_otdf, wrapped_otdf);

        let prepared_pairs = model
            .compute_lodf_pairs(&monitored, &outages)
            .expect("prepared subset pairs");
        let wrapped_pairs = crate::sensitivity::compute_lodf_pairs(&net, &monitored, &outages)
            .expect("wrapper pairs");
        assert_eq!(prepared_pairs, wrapped_pairs);

        let mut lodf_columns = model.lodf_columns();
        let prepared_column = lodf_columns
            .compute_column(&monitored, outages[0])
            .expect("prepared outage column");
        for (row, &value) in prepared_column.iter().enumerate() {
            let matrix_value = prepared_subset[(row, 0)];
            assert!(
                (value - matrix_value).abs() < 1e-12
                    || (value.is_infinite() && matrix_value.is_infinite())
            );
        }

        let mut prepared_n2_columns = model
            .n2_lodf_columns(&all_branches, &all_branches)
            .expect("prepared N-2 columns");
        let prepared_n2 = prepared_n2_columns
            .compute_column(0, 2)
            .expect("prepared N-2 column");
        let wrapped_n2 = crate::sensitivity::compute_n2_lodf(
            &net,
            &crate::sensitivity::N2LodfRequest::new((0, 2)).with_monitored_branches(&all_branches),
        )
        .expect("wrapper N-2");
        assert_eq!(prepared_n2.len(), wrapped_n2.len());
        for (prepared, wrapped) in prepared_n2.iter().zip(wrapped_n2.iter()) {
            assert!(
                (prepared - wrapped).abs() < 1e-12
                    || (prepared.is_infinite() && wrapped.is_infinite())
            );
        }

        let outage_pairs = vec![(0, 2), (2, 0)];
        let prepared_n2_batch = model
            .compute_n2_lodf_batch(&outage_pairs, &all_branches)
            .expect("prepared N-2 batch");
        let wrapped_n2_batch = crate::sensitivity::compute_n2_lodf_batch(
            &net,
            &crate::sensitivity::N2LodfBatchRequest::new(&outage_pairs)
                .with_monitored_branches(&all_branches),
        )
        .expect("wrapper N-2 batch");
        assert_eq!(prepared_n2_batch.n_rows(), wrapped_n2_batch.n_rows());
        assert_eq!(prepared_n2_batch.n_cols(), wrapped_n2_batch.n_cols());
        for i in 0..prepared_n2_batch.n_rows() {
            for j in 0..prepared_n2_batch.n_cols() {
                assert!(
                    (prepared_n2_batch[(i, j)] - wrapped_n2_batch[(i, j)]).abs() < 1e-12
                        || (prepared_n2_batch[(i, j)].is_infinite()
                            && wrapped_n2_batch[(i, j)].is_infinite())
                );
            }
        }
    }

    #[test]
    fn test_dc_sensitivity_workflow_matches_individual_calls() {
        skip_if_no_data!();

        let net = load_net("case14");
        let monitored = vec![0, 3, 7];
        let ptdf_buses = vec![0, 4, 9];
        let outages = vec![1, 5];
        let otdf_buses = vec![0, 4, 9];
        let outage_pairs = vec![(0, 2), (2, 0)];
        let request = DcAnalysisRequest::with_monitored_branches(&monitored)
            .with_ptdf_buses(&ptdf_buses)
            .with_otdf_outages(&outages)
            .with_otdf_buses(&otdf_buses)
            .with_lodf_outages(&outages)
            .with_n2_outage_pairs(&outage_pairs);

        let workflow = run_dc_analysis(&net, &request).expect("workflow");
        let standalone_pf = solve_dc(&net).expect("standalone solve");
        let standalone_ptdf = crate::sensitivity::compute_ptdf(
            &net,
            &crate::sensitivity::PtdfRequest::for_branches(&monitored)
                .with_bus_indices(&ptdf_buses),
        )
        .expect("standalone ptdf");
        let standalone_otdf = crate::sensitivity::compute_otdf(
            &net,
            &crate::sensitivity::OtdfRequest::new(&monitored, &outages)
                .with_bus_indices(&otdf_buses),
        )
        .expect("standalone otdf");
        let standalone_lodf = crate::sensitivity::compute_lodf(
            &net,
            &crate::sensitivity::LodfRequest::for_branches(&monitored, &outages),
        )
        .expect("standalone lodf");
        let standalone_n2 = crate::sensitivity::compute_n2_lodf_batch(
            &net,
            &crate::sensitivity::N2LodfBatchRequest::new(&outage_pairs)
                .with_monitored_branches(&monitored),
        )
        .expect("standalone n2");

        assert_eq!(workflow.monitored_branch_indices, monitored);
        assert_eq!(workflow.ptdf_bus_indices, ptdf_buses);
        assert_eq!(workflow.power_flow.theta, standalone_pf.theta);
        assert_eq!(
            workflow.power_flow.branch_p_flow,
            standalone_pf.branch_p_flow
        );
        assert_eq!(workflow.ptdf, standalone_ptdf);
        assert_eq!(
            workflow.otdf_outage_branch_indices, outages,
            "OTDF outage metadata should preserve request order"
        );
        assert_eq!(
            workflow.otdf_bus_indices, otdf_buses,
            "OTDF bus metadata should preserve request order"
        );
        assert_eq!(
            workflow.lodf_outage_branch_indices, outages,
            "LODF outage metadata should preserve request order"
        );
        assert_eq!(
            workflow.n2_outage_pairs, outage_pairs,
            "N-2 outage-pair metadata should preserve request order"
        );

        let workflow_otdf = workflow.otdf.expect("workflow otdf");
        assert_eq!(workflow_otdf, standalone_otdf);

        let workflow_lodf = workflow.lodf.expect("workflow lodf");
        assert_eq!(workflow_lodf.n_rows(), standalone_lodf.n_rows());
        assert_eq!(workflow_lodf.n_cols(), standalone_lodf.n_cols());
        for i in 0..workflow_lodf.n_rows() {
            for j in 0..workflow_lodf.n_cols() {
                assert!(
                    (workflow_lodf[(i, j)] - standalone_lodf[(i, j)]).abs() < 1e-12
                        || (workflow_lodf[(i, j)].is_infinite()
                            && standalone_lodf[(i, j)].is_infinite())
                );
            }
        }

        let workflow_n2 = workflow.n2_lodf.expect("workflow n2");
        assert_eq!(workflow_n2.n_rows(), standalone_n2.n_rows());
        assert_eq!(workflow_n2.n_cols(), standalone_n2.n_cols());
        for i in 0..workflow_n2.n_rows() {
            for j in 0..workflow_n2.n_cols() {
                assert!(
                    (workflow_n2[(i, j)] - standalone_n2[(i, j)]).abs() < 1e-12
                        || (workflow_n2[(i, j)].is_infinite()
                            && standalone_n2[(i, j)].is_infinite())
                );
            }
        }
    }

    #[test]
    fn test_dc_sensitivity_workflow_headroom_slack_matches_individual_calls() {
        skip_if_no_data!();

        let net = load_net("case14");
        let monitored = vec![0, 3, 7];
        let outages = vec![1, 5];
        let otdf_buses = vec![0, 4, 9];
        let outage_pairs = vec![(0, 2), (2, 0)];
        let participating_buses: Vec<usize> = {
            let bus_map = net.bus_index_map();
            net.generators
                .iter()
                .filter(|g| g.in_service)
                .filter_map(|g| bus_map.get(&g.bus).copied())
                .collect()
        };
        let pf_options = DcPfOptions::with_headroom_slack(&participating_buses);
        let sensitivity_options =
            crate::sensitivity::DcSensitivityOptions::with_headroom_slack(&participating_buses);

        let request = DcAnalysisRequest::with_monitored_branches(&monitored)
            .with_otdf_outages(&outages)
            .with_otdf_buses(&otdf_buses)
            .with_lodf_outages(&outages)
            .with_n2_outage_pairs(&outage_pairs)
            .with_pf_options(pf_options.clone());

        let workflow = run_dc_analysis(&net, &request).expect("workflow");
        let standalone_pf = solve_dc_opts(&net, &pf_options).expect("standalone solve");
        let standalone_ptdf = crate::sensitivity::compute_ptdf(
            &net,
            &crate::sensitivity::PtdfRequest::for_branches(&monitored)
                .with_options(sensitivity_options.clone()),
        )
        .expect("standalone ptdf");
        let standalone_otdf = crate::sensitivity::compute_otdf(
            &net,
            &crate::sensitivity::OtdfRequest::new(&monitored, &outages)
                .with_bus_indices(&otdf_buses)
                .with_options(sensitivity_options.clone()),
        )
        .expect("standalone otdf");
        let standalone_lodf = crate::sensitivity::compute_lodf(
            &net,
            &crate::sensitivity::LodfRequest::for_branches(&monitored, &outages),
        )
        .expect("standalone lodf");
        let standalone_n2 = crate::sensitivity::compute_n2_lodf_batch(
            &net,
            &crate::sensitivity::N2LodfBatchRequest::new(&outage_pairs)
                .with_monitored_branches(&monitored),
        )
        .expect("standalone n2");

        assert_eq!(workflow.power_flow.theta, standalone_pf.theta);
        assert_eq!(
            workflow.power_flow.branch_p_flow,
            standalone_pf.branch_p_flow
        );
        assert_eq!(workflow.ptdf, standalone_ptdf);
        let workflow_otdf = workflow.otdf.as_ref().expect("workflow otdf");
        assert_eq!(workflow_otdf, &standalone_otdf);

        let workflow_lodf = workflow.lodf.as_ref().expect("workflow lodf");
        assert_eq!(workflow_lodf.n_rows(), standalone_lodf.n_rows());
        assert_eq!(workflow_lodf.n_cols(), standalone_lodf.n_cols());
        for i in 0..workflow_lodf.n_rows() {
            for j in 0..workflow_lodf.n_cols() {
                assert!(
                    (workflow_lodf[(i, j)] - standalone_lodf[(i, j)]).abs() < 1e-12
                        || (workflow_lodf[(i, j)].is_infinite()
                            && standalone_lodf[(i, j)].is_infinite())
                );
            }
        }

        let workflow_n2 = workflow.n2_lodf.as_ref().expect("workflow n2");
        assert_eq!(workflow_n2.n_rows(), standalone_n2.n_rows());
        assert_eq!(workflow_n2.n_cols(), standalone_n2.n_cols());
        for i in 0..workflow_n2.n_rows() {
            for j in 0..workflow_n2.n_cols() {
                assert!(
                    (workflow_n2[(i, j)] - standalone_n2[(i, j)]).abs() < 1e-12
                        || (workflow_n2[(i, j)].is_infinite()
                            && standalone_n2[(i, j)].is_infinite())
                );
            }
        }

        let single_slack_ptdf = crate::sensitivity::compute_ptdf(
            &net,
            &crate::sensitivity::PtdfRequest::for_branches(&monitored),
        )
        .expect("single-slack ptdf");
        let workflow_row = workflow.ptdf.row(monitored[0]).expect("workflow ptdf row");
        let single_row = single_slack_ptdf
            .row(monitored[0])
            .expect("single-slack ptdf row");
        assert!(
            workflow_row
                .iter()
                .zip(single_row.iter())
                .any(|(workflow_value, single_value)| (workflow_value - single_value).abs() > 1e-9),
            "headroom-slack workflow PTDF should differ from single-slack PTDF"
        );
    }

    #[test]
    fn test_dc_sensitivity_workflow_slack_weights_matches_individual_calls() {
        skip_if_no_data!();

        let net = load_net("case14");
        let monitored = vec![0, 3, 7];
        let outages = vec![1, 5];
        let otdf_buses = vec![0, 4, 9];
        let outage_pairs = vec![(0, 2), (2, 0)];
        let sensitivity_options = crate::sensitivity::DcSensitivityOptions::with_slack_weights(&[
            (1usize, 3.0),
            (2usize, 1.0),
        ]);

        let request = DcAnalysisRequest::with_monitored_branches(&monitored)
            .with_otdf_outages(&outages)
            .with_otdf_buses(&otdf_buses)
            .with_lodf_outages(&outages)
            .with_n2_outage_pairs(&outage_pairs)
            .with_sensitivity_options(sensitivity_options.clone());

        let workflow = run_dc_analysis(&net, &request).expect("workflow");
        let standalone_pf = solve_dc(&net).expect("standalone solve");
        let standalone_ptdf = crate::sensitivity::compute_ptdf(
            &net,
            &crate::sensitivity::PtdfRequest::for_branches(&monitored)
                .with_options(sensitivity_options.clone()),
        )
        .expect("standalone ptdf");
        let standalone_otdf = crate::sensitivity::compute_otdf(
            &net,
            &crate::sensitivity::OtdfRequest::new(&monitored, &outages)
                .with_bus_indices(&otdf_buses)
                .with_options(sensitivity_options.clone()),
        )
        .expect("standalone otdf");
        let standalone_lodf = crate::sensitivity::compute_lodf(
            &net,
            &crate::sensitivity::LodfRequest::for_branches(&monitored, &outages),
        )
        .expect("standalone lodf");
        let standalone_n2 = crate::sensitivity::compute_n2_lodf_batch(
            &net,
            &crate::sensitivity::N2LodfBatchRequest::new(&outage_pairs)
                .with_monitored_branches(&monitored),
        )
        .expect("standalone n2");

        assert_eq!(workflow.power_flow.theta, standalone_pf.theta);
        assert_eq!(
            workflow.power_flow.branch_p_flow,
            standalone_pf.branch_p_flow
        );
        assert_eq!(workflow.ptdf, standalone_ptdf);
        assert_eq!(
            workflow.otdf.as_ref().expect("workflow otdf"),
            &standalone_otdf
        );

        let workflow_lodf = workflow.lodf.as_ref().expect("workflow lodf");
        assert_eq!(workflow_lodf.n_rows(), standalone_lodf.n_rows());
        assert_eq!(workflow_lodf.n_cols(), standalone_lodf.n_cols());
        for i in 0..workflow_lodf.n_rows() {
            for j in 0..workflow_lodf.n_cols() {
                assert!(
                    (workflow_lodf[(i, j)] - standalone_lodf[(i, j)]).abs() < 1e-12
                        || (workflow_lodf[(i, j)].is_infinite()
                            && standalone_lodf[(i, j)].is_infinite())
                );
            }
        }

        let workflow_n2 = workflow.n2_lodf.as_ref().expect("workflow n2");
        assert_eq!(workflow_n2.n_rows(), standalone_n2.n_rows());
        assert_eq!(workflow_n2.n_cols(), standalone_n2.n_cols());
        for i in 0..workflow_n2.n_rows() {
            for j in 0..workflow_n2.n_cols() {
                assert!(
                    (workflow_n2[(i, j)] - standalone_n2[(i, j)]).abs() < 1e-12
                        || (workflow_n2[(i, j)].is_infinite()
                            && standalone_n2[(i, j)].is_infinite())
                );
            }
        }

        let single_slack_ptdf = crate::sensitivity::compute_ptdf(
            &net,
            &crate::sensitivity::PtdfRequest::for_branches(&monitored),
        )
        .expect("single-slack ptdf");
        let workflow_row = workflow.ptdf.row(monitored[0]).expect("workflow ptdf row");
        let single_row = single_slack_ptdf
            .row(monitored[0])
            .expect("single-slack ptdf row");
        assert!(
            workflow_row
                .iter()
                .zip(single_row.iter())
                .any(|(workflow_value, single_value)| (workflow_value - single_value).abs() > 1e-9),
            "weighted workflow PTDF should differ from single-slack PTDF"
        );
    }

    /// PST-02: Distributed slack on case14 among buses 1, 2, 8 (external bus numbers).
    ///
    /// Distribution is headroom-based: each non-slack participant absorbs
    /// (headroom_k / total_headroom) × mismatch, up to available capacity.
    /// The slack bus absorbs the residual and is included in slack_distribution.
    ///
    /// Verifies:
    ///   - slack_distribution has 3 entries (one per declared participant).
    ///   - Each non-slack participant's share does not exceed its generator headroom.
    ///   - Sum of all slack_distribution values equals the single-slack mismatch.
    ///   - total_generation_mw = total_load + remaining_slack (lossless DC PF identity).
    #[test]
    fn test_headroom_slack_case14() {
        skip_if_no_data!();
        let net = load_net("case14");

        let bus_map = net.bus_index_map();
        let idx1 = bus_map[&1]; // bus number 1 -> internal idx 0 (slack bus)
        let idx2 = bus_map[&2]; // bus number 2 -> internal idx 1
        let idx8 = bus_map[&8]; // bus number 8 -> internal idx 7

        let opts = DcPfOptions::with_headroom_slack(&[idx1, idx2, idx8]);
        let result = solve_dc_opts(&net, &opts).expect("headroom slack DC PF should converge");

        let single = solve_dc(&net).expect("single-slack DC PF");
        let single_mismatch_mw = single.slack_p_injection * net.base_mva;

        // 1. slack_distribution has one entry per declared participant (including
        //    the slack bus, which records its residual absorption).
        assert_eq!(
            result.slack_distribution.len(),
            3,
            "Expected 3 entries in slack_distribution (one per declared participant)"
        );

        // 2. Each non-slack participant's share must not exceed its headroom.
        for (&bidx, &share_mw) in &result.slack_distribution {
            if bidx == idx1 {
                continue; // slack bus residual — no headroom constraint
            }
            let headroom_mw: f64 = net
                .generators
                .iter()
                .filter(|g| g.in_service && bus_map.get(&g.bus) == Some(&bidx))
                .map(|g| (g.pmax - g.p).max(0.0))
                .sum();
            assert!(
                share_mw.abs() <= headroom_mw + 1e-6,
                "Bus {bidx}: share {share_mw:.4} MW exceeds headroom {headroom_mw:.4} MW"
            );
        }

        // 3. Sum of all slack_distribution values (including slack residual) equals
        //    the original single-slack mismatch (conservation).
        let total_absorbed: f64 = result.slack_distribution.values().sum();
        assert!(
            (total_absorbed - single_mismatch_mw).abs() < 1e-4,
            "Total distributed ({total_absorbed:.4} MW) should equal \
             single-slack mismatch ({single_mismatch_mw:.4} MW)"
        );

        // 4. total_generation_mw = total_load + remaining_slack (lossless DC PF identity).
        let total_load = net.total_load_mw();
        let remaining_slack_mw = result.slack_p_injection * net.base_mva;
        assert!(
            (result.total_generation_mw - (total_load + remaining_slack_mw)).abs() < 1e-4,
            "total_generation_mw ({:.4}) should equal total_load + remaining_slack ({:.4})",
            result.total_generation_mw,
            total_load + remaining_slack_mw
        );
    }

    /// Participation-factor distributed slack on case14.
    ///
    /// Sets AGC participation factors on generators at buses 2 and 3, then
    /// verifies:
    ///   - Mismatch is distributed proportionally to participation weights.
    ///   - Total absorbed equals single-slack mismatch (conservation).
    ///   - Branch flows differ from single-slack (injection profile changed).
    ///   - D-PTDF computed with matching SlackWeights reproduces branch flows.
    #[test]
    fn test_participation_factor_slack_case14() {
        skip_if_no_data!();
        let mut net = load_net("case14");
        let bus_map = net.bus_index_map();
        let idx2 = bus_map[&2];
        let idx3 = bus_map[&3];

        // Assign participation factors: bus 2 gets 3x the weight of bus 3.
        for g in &mut net.generators {
            if let Some(&bidx) = bus_map.get(&g.bus) {
                if bidx == idx2 {
                    g.agc_participation_factor = Some(0.75);
                } else if bidx == idx3 {
                    g.agc_participation_factor = Some(0.25);
                }
            }
        }

        let weights = net.agc_participation_by_bus();
        assert!(
            !weights.is_empty(),
            "Should have participation factors from generators"
        );

        let opts = DcPfOptions::with_participation_factors(&weights);
        let result = solve_dc_opts(&net, &opts).expect("participation factor DC PF");
        let single = solve_dc(&net).expect("single-slack DC PF");
        let single_mismatch_mw = single.slack_p_injection * net.base_mva;

        // 1. Mismatch is fully distributed (slack residual should be near-zero
        //    since participation factors absorb everything unlike headroom which
        //    can be exhausted).
        let total_absorbed: f64 = result.slack_distribution.values().sum();
        assert!(
            (total_absorbed - single_mismatch_mw).abs() < 1e-4,
            "Total distributed ({total_absorbed:.4} MW) should equal \
             single-slack mismatch ({single_mismatch_mw:.4} MW)"
        );

        // 2. Non-slack participants received shares proportional to weights.
        let bus2_share = result.slack_distribution.get(&idx2).copied().unwrap_or(0.0);
        let bus3_share = result.slack_distribution.get(&idx3).copied().unwrap_or(0.0);
        let non_slack_total = bus2_share + bus3_share;
        if non_slack_total.abs() > 1e-6 {
            let ratio = bus2_share / non_slack_total;
            assert!(
                (ratio - 0.75).abs() < 0.02,
                "Bus 2 should get ~75% of distributed slack, got {:.1}%",
                ratio * 100.0
            );
        }

        // 3. Branch flows should differ from single-slack (different injection profile).
        let max_diff: f64 = result
            .branch_p_flow
            .iter()
            .zip(single.branch_p_flow.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f64, f64::max);
        assert!(
            max_diff > 1e-6,
            "Participation-factor flows should differ from single-slack flows"
        );

        // 4. D-PTDF with matching SlackWeights should reproduce branch flows from
        //    the participation-factor solve.
        let sens_opts = crate::sensitivity::DcSensitivityOptions::with_slack_weights(&weights);
        let all_branches: Vec<usize> = (0..net.n_branches()).collect();
        let mut study = PreparedDcStudy::new(&net).expect("prepared study");
        let dptdf = study
            .compute_ptdf_with_options(&all_branches, None, &sens_opts)
            .expect("D-PTDF");
        // D-PTDF * P_inj should approximate branch flows.
        for (br_idx, &actual_flow) in result.branch_p_flow.iter().enumerate() {
            if let Some(row) = dptdf.row(br_idx) {
                let predicted: f64 = row
                    .iter()
                    .enumerate()
                    .map(|(col_pos, &ptdf_val)| {
                        let bus_idx = dptdf.bus_indices()[col_pos];
                        ptdf_val * result.p_inject_pu[bus_idx]
                    })
                    .sum();
                assert!(
                    (predicted - actual_flow).abs() < 1e-6,
                    "D-PTDF flow reconstruction mismatch on branch {br_idx}: \
                     predicted={predicted:.8}, actual={actual_flow:.8}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // MATPOWER DC-PF reference regression tests
    // -----------------------------------------------------------------------

    /// MATPOWER DC-PF reference regression test — case9.
    /// Theta and branch flows validated against MATPOWER 7.1 rundcpf('case9').
    #[test]
    fn test_dc_case9_matpower_reference() {
        skip_if_no_data!();
        let net = load_net("case9");
        let result = solve_dc(&net).expect("case9 DC PF should converge");

        // Reference bus angles (degrees) — MATPOWER 7.1 rundcpf('case9')
        // Bus order: 1, 2, 3, 4, 5, 6, 7, 8, 9
        let ref_theta_deg: &[f64] = &[
            0.0000000000,  // bus 1 (slack)
            9.7960188551,  // bus 2
            5.0605600451,  // bus 3
            -2.2111587230, // bus 4
            -3.7380912470, // bus 5
            2.2066572676,  // bus 6
            0.8224410570,  // bus 7
            3.9590113172,  // bus 8
            -4.0634004908, // bus 9
        ];

        assert_eq!(
            result.theta.len(),
            ref_theta_deg.len(),
            "case9: expected {} buses, got {}",
            ref_theta_deg.len(),
            result.theta.len()
        );

        for (i, &theta) in result.theta.iter().enumerate() {
            let theta_deg = theta.to_degrees();
            assert!(
                (theta_deg - ref_theta_deg[i]).abs() < 1e-2,
                "case9 bus {} theta: got {:.4} deg, expected {:.4} deg",
                net.buses[i].number,
                theta_deg,
                ref_theta_deg[i]
            );
        }

        // Reference branch flows (p.u. on 100 MVA base) — MATPOWER 7.1
        // Branch order: 1->4, 4->5, 5->6, 3->6, 6->7, 7->8, 8->2, 8->9, 9->4
        let ref_flows: &[f64] = &[
            0.6700000000,  // branch 0: 1->4
            0.2896739130,  // branch 1: 4->5
            -0.6103260870, // branch 2: 5->6
            0.8500000000,  // branch 3: 3->6
            0.2396739130,  // branch 4: 6->7
            -0.7603260870, // branch 5: 7->8
            -1.6300000000, // branch 6: 8->2
            0.8696739130,  // branch 7: 8->9
            -0.3803260870, // branch 8: 9->4
        ];

        assert_eq!(
            result.branch_p_flow.len(),
            ref_flows.len(),
            "case9: expected {} branches, got {}",
            ref_flows.len(),
            result.branch_p_flow.len()
        );

        for (i, &flow) in result.branch_p_flow.iter().enumerate() {
            assert!(
                (flow - ref_flows[i]).abs() < 1e-4,
                "case9 branch {} ({}->{}) flow: got {:.6}, expected {:.6}",
                i,
                net.branches[i].from_bus,
                net.branches[i].to_bus,
                flow,
                ref_flows[i]
            );
        }
    }

    /// MATPOWER DC-PF reference regression test — case14.
    /// Theta and branch flows validated against MATPOWER 7.1 rundcpf('case14').
    #[test]
    fn test_dc_case14_matpower_reference() {
        skip_if_no_data!();
        let net = load_net("case14");
        let result = solve_dc(&net).expect("case14 DC PF should converge");

        // Reference bus angles (degrees) — MATPOWER 7.1 rundcpf('case14')
        // Bus order: 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14
        let ref_theta_deg: &[f64] = &[
            0.0000000000,   // bus 1 (slack)
            -5.0120111659,  // bus 2
            -12.9536631292, // bus 3
            -10.5836674351, // bus 4
            -9.0938942486,  // bus 5
            -14.8520790526, // bus 6
            -13.9070545899, // bus 7
            -13.9070545899, // bus 8
            -15.6946888799, // bus 9
            -15.9741231351, // bus 10
            -15.6188501240, // bus 11
            -15.9670768584, // bus 12
            -16.1397037396, // bus 13
            -17.1882875703, // bus 14
        ];

        assert_eq!(
            result.theta.len(),
            ref_theta_deg.len(),
            "case14: expected {} buses, got {}",
            ref_theta_deg.len(),
            result.theta.len()
        );

        for (i, &theta) in result.theta.iter().enumerate() {
            let theta_deg = theta.to_degrees();
            assert!(
                (theta_deg - ref_theta_deg[i]).abs() < 1e-2,
                "case14 bus {} theta: got {:.4} deg, expected {:.4} deg",
                net.buses[i].number,
                theta_deg,
                ref_theta_deg[i]
            );
        }

        // Reference branch flows (p.u. on 100 MVA base) — MATPOWER 7.1
        // 20 branches in case14
        let ref_flows: &[f64] = &[
            1.4783859556,  // branch 0:  1->2
            0.7116140444,  // branch 1:  1->5
            0.7001463596,  // branch 2:  2->3
            0.5515185270,  // branch 3:  2->4
            0.4097210690,  // branch 4:  2->5
            -0.2418536404, // branch 5:  3->4
            -0.6174649065, // branch 6:  4->5
            0.2836115279,  // branch 7:  4->7
            0.1655182652,  // branch 8:  4->9
            0.4278702069,  // branch 9:  5->6
            0.0672834580,  // branch 10: 6->11
            0.0760735814,  // branch 11: 6->12
            0.1725131674,  // branch 12: 6->13
            0.0000000000,  // branch 13: 7->8
            0.2836115279,  // branch 14: 7->9
            0.0577165420,  // branch 15: 9->10
            0.0964132512,  // branch 16: 9->14
            -0.0322834580, // branch 17: 10->11
            0.0150735814,  // branch 18: 12->13
            0.0525867488,  // branch 19: 13->14
        ];

        assert_eq!(
            result.branch_p_flow.len(),
            ref_flows.len(),
            "case14: expected {} branches, got {}",
            ref_flows.len(),
            result.branch_p_flow.len()
        );

        for (i, &flow) in result.branch_p_flow.iter().enumerate() {
            assert!(
                (flow - ref_flows[i]).abs() < 1e-4,
                "case14 branch {} ({}->{}) flow: got {:.6}, expected {:.6}",
                i,
                net.branches[i].from_bus,
                net.branches[i].to_bus,
                flow,
                ref_flows[i]
            );
        }
    }
}
