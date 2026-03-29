// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Distribution Factor Analysis (DFAX) engine used by the public matrix,
//! injection-capability, AFC, and simultaneous-transfer surfaces.
//!
//! These quantities are used by commercial transmission planning tools (PowerGEM TARA,
//! GE PlanOS) to evaluate transfer limits, interconnection capability, and flowgate
//! constraints under N-0 and N-1 conditions.
//!
//! All formulas use the DC power flow approximation (flat voltage, small angles, lossless).
//!
//! # Conventions
//!
//! - PTDF is n_branches × n_buses with the slack bus column = 0.
//! - LODF is n_branches × n_branches with LODF\[l,l\] = -1 and bridge lines = ±∞.
//! - Flows and ratings are in per-unit (divide by base_mva if working in MW).
//! - ATC / injection capability are returned in the same units as the supplied ratings.

use faer::Mat;
use std::collections::{HashMap, HashSet};
use surge_dc::DcError;
use surge_dc::{DcPfOptions, PreparedDcStudy, PtdfRequest, PtdfRows};
use surge_network::Network;
use tracing::{info, warn};

use crate::error::TransferError;
use crate::injection::{InjectionCapabilityMap, InjectionCapabilityOptions};
use crate::matrices::{BldfMatrix, GsfMatrix};
use crate::types::{
    AfcRequest, AfcResult, MultiTransferRequest, MultiTransferResult, NercAtcLimitCause,
    NercAtcRequest, NercAtcResult, TransferPath,
};

pub(crate) fn validate_transfer_path(
    network: &Network,
    path: &TransferPath,
) -> Result<(), TransferError> {
    if path.name.trim().is_empty() {
        return Err(TransferError::InvalidTransferPath {
            name: path.name.clone(),
            reason: "path name must not be empty".to_string(),
        });
    }
    if path.source_buses.is_empty() {
        return Err(TransferError::InvalidTransferPath {
            name: path.name.clone(),
            reason: "source_buses must not be empty".to_string(),
        });
    }
    if path.sink_buses.is_empty() {
        return Err(TransferError::InvalidTransferPath {
            name: path.name.clone(),
            reason: "sink_buses must not be empty".to_string(),
        });
    }

    let bus_map = network.bus_index_map();
    let mut seen_sources = HashSet::new();
    for &bus in &path.source_buses {
        if !seen_sources.insert(bus) {
            return Err(TransferError::InvalidTransferPath {
                name: path.name.clone(),
                reason: format!("duplicate source bus {bus}"),
            });
        }
        if !bus_map.contains_key(&bus) {
            return Err(TransferError::InvalidTransferPath {
                name: path.name.clone(),
                reason: format!("source bus {bus} not found in network"),
            });
        }
    }

    let mut seen_sinks = HashSet::new();
    for &bus in &path.sink_buses {
        if !seen_sinks.insert(bus) {
            return Err(TransferError::InvalidTransferPath {
                name: path.name.clone(),
                reason: format!("duplicate sink bus {bus}"),
            });
        }
        if !bus_map.contains_key(&bus) {
            return Err(TransferError::InvalidTransferPath {
                name: path.name.clone(),
                reason: format!("sink bus {bus} not found in network"),
            });
        }
        if seen_sources.contains(&bus) {
            return Err(TransferError::InvalidTransferPath {
                name: path.name.clone(),
                reason: format!("bus {bus} cannot be both source and sink"),
            });
        }
    }

    Ok(())
}

/// Prepared transfer-sensitivity model for repeated DFAX-style studies.
pub struct PreparedTransferModel<'a> {
    network: &'a Network,
    dc_model: PreparedDcStudy<'a>,
    base_flows_pu: Vec<f64>,
    all_branch_ptdf: Option<PtdfRows>,
}

impl<'a> PreparedTransferModel<'a> {
    /// Prepare reusable PTDF/LODF state and base-case DC flows for one network.
    pub fn new(network: &'a Network) -> Result<Self, DcError> {
        let mut dc_model = PreparedDcStudy::new(network)?;
        let base_flows_pu = dc_model.solve(&DcPfOptions::default())?.branch_p_flow;
        Ok(Self {
            network,
            dc_model,
            base_flows_pu,
            all_branch_ptdf: None,
        })
    }

    pub(crate) fn from_base_state(
        network: &'a Network,
        base_flows_pu: Vec<f64>,
    ) -> Result<Self, DcError> {
        let dc_model = PreparedDcStudy::new(network)?;
        Ok(Self {
            network,
            dc_model,
            base_flows_pu,
            all_branch_ptdf: None,
        })
    }

    fn default_monitored_branches(&self) -> Vec<usize> {
        self.network
            .branches
            .iter()
            .enumerate()
            .filter(|(_, branch)| branch.in_service && branch.rating_a_mva > 0.0)
            .map(|(idx, _)| idx)
            .collect()
    }

    fn default_contingency_branches(&self) -> Vec<usize> {
        self.network
            .branches
            .iter()
            .enumerate()
            .filter(|(_, branch)| branch.in_service && branch.x.abs() >= 1e-20)
            .map(|(idx, _)| idx)
            .collect()
    }

    fn branch_ratings_pu(&self) -> Vec<f64> {
        self.network
            .branches
            .iter()
            .map(|branch| {
                if branch.in_service && branch.rating_a_mva > 0.0 {
                    branch.rating_a_mva / self.network.base_mva
                } else {
                    f64::INFINITY
                }
            })
            .collect()
    }

    fn all_branch_ptdf(&mut self) -> Result<&PtdfRows, DcError> {
        if self.all_branch_ptdf.is_none() {
            let all_branches: Vec<usize> = (0..self.network.n_branches()).collect();
            self.all_branch_ptdf = Some(self.dc_model.compute_ptdf(&all_branches)?);
        }
        Ok(self
            .all_branch_ptdf
            .as_ref()
            .expect("all-branch PTDF cache initialized"))
    }

    fn validate_path(&self, path: &TransferPath) -> Result<(), TransferError> {
        validate_transfer_path(self.network, path)
    }

    fn validate_branch_indices(&self, indices: &[usize], label: &str) -> Result<(), TransferError> {
        let n_branches = self.network.n_branches();
        let mut seen = HashSet::new();
        for &branch_idx in indices {
            if branch_idx >= n_branches {
                return Err(TransferError::InvalidRequest(format!(
                    "{label} branch index {branch_idx} out of range (n_branches = {n_branches})"
                )));
            }
            if !seen.insert(branch_idx) {
                return Err(TransferError::InvalidRequest(format!(
                    "{label} contains duplicate branch index {branch_idx}"
                )));
            }
        }
        Ok(())
    }

    fn interface_net_ptdf(
        &mut self,
        branch_indices: &[usize],
        path: &TransferPath,
    ) -> Result<HashMap<usize, f64>, TransferError> {
        self.validate_path(path)?;
        let bus_map = self.network.bus_index_map();
        let interface_bus_indices = collect_interface_bus_indices(
            &bus_map,
            path.source_buses.iter().chain(path.sink_buses.iter()),
        );
        let ptdf = self.dc_model.compute_ptdf_request(
            &PtdfRequest::for_branches(branch_indices).with_bus_indices(&interface_bus_indices),
        )?;

        let n_from = path.source_buses.len() as f64;
        let n_to = path.sink_buses.len() as f64;
        let mut net_ptdf = HashMap::with_capacity(branch_indices.len());

        for &branch_idx in branch_indices {
            let from_sum: f64 = path
                .source_buses
                .iter()
                .filter_map(|bus| bus_map.get(bus))
                .map(|&bus_idx| ptdf.get(branch_idx, bus_idx))
                .sum();
            let to_sum: f64 = path
                .sink_buses
                .iter()
                .filter_map(|bus| bus_map.get(bus))
                .map(|&bus_idx| ptdf.get(branch_idx, bus_idx))
                .sum();
            net_ptdf.insert(branch_idx, from_sum / n_from - to_sum / n_to);
        }

        Ok(net_ptdf)
    }

    pub fn compute_gsf(&mut self) -> Result<GsfMatrix, TransferError> {
        let bus_map = self.network.bus_index_map();
        let n_br = self.network.n_branches();
        let in_service_gens: Vec<&surge_network::network::Generator> = self
            .network
            .generators
            .iter()
            .filter(|g| g.in_service)
            .collect();
        let n_gen = in_service_gens.len();
        let ptdf = self.all_branch_ptdf()?;

        let mut values = Mat::<f64>::zeros(n_br, n_gen);
        let branch_ids: Vec<usize> = (0..n_br).collect();
        let mut gen_buses = Vec::with_capacity(n_gen);

        for (g, generator) in in_service_gens.iter().enumerate() {
            gen_buses.push(generator.bus);
            if let Some(&b_idx) = bus_map.get(&generator.bus) {
                for l in 0..n_br {
                    values[(l, g)] = ptdf.get(l, b_idx);
                }
            }
        }

        Ok(GsfMatrix {
            values,
            gen_buses,
            branch_ids,
        })
    }

    pub fn compute_bldf(&mut self) -> Result<BldfMatrix, TransferError> {
        let n_bus = self.network.n_buses();
        let n_br = self.network.n_branches();
        let ptdf = self.all_branch_ptdf()?;

        let mut values = Mat::<f64>::zeros(n_bus, n_br);
        for b in 0..n_bus {
            for l in 0..n_br {
                values[(b, l)] = -ptdf.get(l, b);
            }
        }

        Ok(BldfMatrix { values })
    }

    pub fn compute_injection_capability(
        &mut self,
        options: &InjectionCapabilityOptions,
    ) -> Result<InjectionCapabilityMap, TransferError> {
        let n_bus = self.network.n_buses();
        let n_br = self.network.n_branches();
        let slack_idx = self.network.slack_bus_index().unwrap_or(usize::MAX);
        let ratings = self.branch_ratings_pu();

        let mut monitored = options
            .monitored_branches
            .clone()
            .unwrap_or_else(|| self.default_monitored_branches());
        monitored.sort_unstable();
        monitored.dedup();
        self.validate_branch_indices(&monitored, "monitored")?;

        let mut contingencies = options
            .contingency_branches
            .clone()
            .unwrap_or_else(|| self.default_contingency_branches());
        contingencies.sort_unstable();
        contingencies.dedup();
        self.validate_branch_indices(&contingencies, "contingency")?;

        let mut required_ptdf = monitored.clone();
        required_ptdf.extend_from_slice(&contingencies);
        required_ptdf.sort_unstable();
        required_ptdf.dedup();
        let distributed_slack = options.sensitivity_options.is_some();
        let ptdf = if let Some(ref sens_opts) = options.sensitivity_options {
            self.dc_model
                .compute_ptdf_with_options(&required_ptdf, None, sens_opts)?
        } else {
            self.dc_model.compute_ptdf(&required_ptdf)?
        };

        let mut failed_contingencies: Vec<usize> = Vec::new();
        let mut trlim_by_bus = vec![f64::INFINITY; n_bus];
        let sensitivity_options = options.sensitivity_options.as_ref();
        let bus_map = self.network.bus_index_map();
        let zero_affected_bus_limits = |k: usize, trlim_by_bus: &mut [f64]| {
            let mut zeroed_any = false;
            if let Some(row) = ptdf.row(k) {
                for (b, bus_limit) in trlim_by_bus.iter_mut().enumerate().take(n_bus) {
                    if !distributed_slack && b == slack_idx {
                        continue;
                    }
                    if row.get(b).copied().unwrap_or(0.0).abs() > 1e-12 {
                        *bus_limit = 0.0;
                        zeroed_any = true;
                    }
                }
            }
            if zeroed_any {
                return;
            }
            if let Some(branch) = self.network.branches.get(k) {
                for bus_number in [branch.from_bus, branch.to_bus] {
                    if let Some(&bus_idx) = bus_map.get(&bus_number) {
                        if distributed_slack || bus_idx != slack_idx {
                            trlim_by_bus[bus_idx] = 0.0;
                        }
                    }
                }
            }
        };

        let exact_fallback_for_outage =
            |k: usize, trlim_by_bus: &mut [f64], failed_contingencies: &mut Vec<usize>| {
                let mut net_ctg = self.network.clone();
                net_ctg.branches[k].in_service = false;

                let mut ctg_model = match PreparedDcStudy::new_unchecked(&net_ctg) {
                    Ok(model) => model,
                    Err(e) => {
                        warn!(contingency_branch = k, error = %e, "exact contingency DC model build failed; zeroing bus limits");
                        failed_contingencies.push(k);
                        zero_affected_bus_limits(k, trlim_by_bus);
                        return;
                    }
                };
                let ctg_dc = match ctg_model.solve(&DcPfOptions::default()) {
                    Ok(solution) => solution,
                    Err(e) => {
                        warn!(contingency_branch = k, error = %e, "exact contingency DC solve failed; zeroing bus limits");
                        failed_contingencies.push(k);
                        zero_affected_bus_limits(k, trlim_by_bus);
                        return;
                    }
                };

                let monitored_after_outage: Vec<usize> =
                    monitored.iter().copied().filter(|&l| l != k).collect();
                if monitored_after_outage.is_empty() {
                    return;
                }
                let exact_ptdf = match sensitivity_options {
                    Some(sens_opts) => match ctg_model.compute_ptdf_with_options(
                        &monitored_after_outage,
                        None,
                        sens_opts,
                    ) {
                        Ok(ptdf) => ptdf,
                        Err(e) => {
                            warn!(contingency_branch = k, error = %e, "exact contingency PTDF computation failed; zeroing bus limits");
                            failed_contingencies.push(k);
                            zero_affected_bus_limits(k, trlim_by_bus);
                            return;
                        }
                    },
                    None => match ctg_model.compute_ptdf(&monitored_after_outage) {
                        Ok(ptdf) => ptdf,
                        Err(e) => {
                            warn!(contingency_branch = k, error = %e, "exact contingency PTDF computation failed; zeroing bus limits");
                            failed_contingencies.push(k);
                            zero_affected_bus_limits(k, trlim_by_bus);
                            return;
                        }
                    },
                };

                for &l in &monitored_after_outage {
                    if l >= n_br || !ratings[l].is_finite() {
                        continue;
                    }
                    let Some(row) = exact_ptdf.row(l) else {
                        continue;
                    };
                    let post_flow = ctg_dc.branch_p_flow.get(l).copied().unwrap_or(0.0);
                    let post_rating = ratings[l] * options.post_contingency_rating_fraction;

                    for (b, bus_limit) in trlim_by_bus.iter_mut().enumerate().take(n_bus) {
                        if !distributed_slack && b == slack_idx {
                            continue;
                        }
                        let post_np = row.get(b).copied().unwrap_or(0.0);
                        if post_np.abs() < 1e-12 {
                            continue;
                        }

                        let (headroom, limit) = if post_np > 0.0 {
                            let h = post_rating - post_flow;
                            (h, h / post_np)
                        } else {
                            let h = post_rating + post_flow;
                            (h, h / (-post_np))
                        };

                        if headroom < 0.0 {
                            *bus_limit = 0.0;
                            continue;
                        }
                        if limit < *bus_limit {
                            *bus_limit = limit;
                        }
                    }
                }
            };

        for &l in &monitored {
            if l >= n_br || !ratings[l].is_finite() {
                continue;
            }

            let Some(row) = ptdf.row(l) else {
                continue;
            };
            let flow_l = self.base_flows_pu.get(l).copied().unwrap_or(0.0);

            for (b, bus_limit) in trlim_by_bus.iter_mut().enumerate().take(n_bus) {
                if !distributed_slack && b == slack_idx {
                    continue;
                }
                let np = row.get(b).copied().unwrap_or(0.0);
                if np.abs() < 1e-12 {
                    continue;
                }

                let (headroom, limit) = if np > 0.0 {
                    let h = ratings[l] - flow_l;
                    (h, h / np)
                } else {
                    let h = ratings[l] + flow_l;
                    (h, h / (-np))
                };

                if headroom < 0.0 {
                    *bus_limit = 0.0;
                    continue;
                }
                if limit < *bus_limit {
                    *bus_limit = limit;
                }
            }
        }

        if options.exact {
            for &k in &contingencies {
                if k >= n_br || !self.network.branches[k].in_service {
                    continue;
                }

                let mut net_ctg = self.network.clone();
                net_ctg.branches[k].in_service = false;

                let mut ctg_model = match PreparedDcStudy::new_unchecked(&net_ctg) {
                    Ok(model) => model,
                    Err(e) => {
                        warn!(contingency_branch = k, error = %e, "exact contingency DC model build failed; zeroing bus limits");
                        failed_contingencies.push(k);
                        zero_affected_bus_limits(k, &mut trlim_by_bus);
                        continue;
                    }
                };
                let ctg_dc = match ctg_model.solve(&DcPfOptions::default()) {
                    Ok(solution) => solution,
                    Err(e) => {
                        warn!(contingency_branch = k, error = %e, "exact contingency DC solve failed; zeroing bus limits");
                        failed_contingencies.push(k);
                        zero_affected_bus_limits(k, &mut trlim_by_bus);
                        continue;
                    }
                };

                let monitored_after_outage: Vec<usize> =
                    monitored.iter().copied().filter(|&l| l != k).collect();
                if monitored_after_outage.is_empty() {
                    continue;
                }
                let exact_ptdf = match sensitivity_options {
                    Some(sens_opts) => {
                        match ctg_model.compute_ptdf_with_options(
                            &monitored_after_outage,
                            None,
                            sens_opts,
                        ) {
                            Ok(ptdf) => ptdf,
                            Err(e) => {
                                warn!(contingency_branch = k, error = %e, "exact contingency PTDF computation failed; zeroing bus limits");
                                failed_contingencies.push(k);
                                zero_affected_bus_limits(k, &mut trlim_by_bus);
                                continue;
                            }
                        }
                    }
                    None => match ctg_model.compute_ptdf(&monitored_after_outage) {
                        Ok(ptdf) => ptdf,
                        Err(e) => {
                            warn!(contingency_branch = k, error = %e, "exact contingency PTDF computation failed; zeroing bus limits");
                            failed_contingencies.push(k);
                            zero_affected_bus_limits(k, &mut trlim_by_bus);
                            continue;
                        }
                    },
                };

                for &l in &monitored_after_outage {
                    if l >= n_br || !ratings[l].is_finite() {
                        continue;
                    }
                    let Some(row) = exact_ptdf.row(l) else {
                        continue;
                    };
                    let post_flow = ctg_dc.branch_p_flow.get(l).copied().unwrap_or(0.0);
                    let post_rating = ratings[l] * options.post_contingency_rating_fraction;

                    for (b, bus_limit) in trlim_by_bus.iter_mut().enumerate().take(n_bus) {
                        if !distributed_slack && b == slack_idx {
                            continue;
                        }
                        let post_np = row.get(b).copied().unwrap_or(0.0);
                        if post_np.abs() < 1e-12 {
                            continue;
                        }

                        let (headroom, limit) = if post_np > 0.0 {
                            let h = post_rating - post_flow;
                            (h, h / post_np)
                        } else {
                            let h = post_rating + post_flow;
                            (h, h / (-post_np))
                        };

                        if headroom < 0.0 {
                            *bus_limit = 0.0;
                            continue;
                        }
                        if limit < *bus_limit {
                            *bus_limit = limit;
                        }
                    }
                }
            }
        } else {
            let mut lodf_columns = self.dc_model.lodf_columns();
            for &k in &contingencies {
                if k >= n_br || !self.network.branches[k].in_service {
                    continue;
                }

                let mut monitored_with_diag = monitored.clone();
                if !monitored_with_diag.contains(&k) {
                    monitored_with_diag.push(k);
                }
                let outage_column = match lodf_columns.compute_column(&monitored_with_diag, k) {
                    Ok(column) => column,
                    Err(e) => {
                        warn!(contingency_branch = k, error = %e, "LODF column computation failed; falling back to exact recomputation");
                        exact_fallback_for_outage(k, &mut trlim_by_bus, &mut failed_contingencies);
                        continue;
                    }
                };
                let diag_pos = match monitored_with_diag.iter().position(|&branch| branch == k) {
                    Some(pos) => pos,
                    None => continue,
                };
                if !outage_column[diag_pos].is_finite()
                    || monitored.iter().enumerate().any(|(monitored_pos, &l)| {
                        l != k && !outage_column[monitored_pos].is_finite()
                    })
                {
                    exact_fallback_for_outage(k, &mut trlim_by_bus, &mut failed_contingencies);
                    continue;
                }

                let contingency_ptdf = ptdf.row(k);
                let contingency_flow = self.base_flows_pu.get(k).copied().unwrap_or(0.0);

                for (monitored_pos, &l) in monitored.iter().enumerate() {
                    if l >= n_br || !ratings[l].is_finite() || l == k {
                        continue;
                    }

                    let lodf_lk = outage_column[monitored_pos];
                    if lodf_lk.abs() > 0.8 {
                        warn!(
                            monitored_branch = l,
                            contingency_branch = k,
                            lodf = lodf_lk,
                            "large LODF (>0.8) — first-order injection capability screening may be inaccurate"
                        );
                    }

                    let Some(monitored_ptdf) = ptdf.row(l) else {
                        continue;
                    };
                    let post_flow = self.base_flows_pu.get(l).copied().unwrap_or(0.0)
                        + lodf_lk * contingency_flow;
                    let post_rating = ratings[l] * options.post_contingency_rating_fraction;

                    for (b, bus_limit) in trlim_by_bus.iter_mut().enumerate().take(n_bus) {
                        if !distributed_slack && b == slack_idx {
                            continue;
                        }
                        let monitored_np = monitored_ptdf.get(b).copied().unwrap_or(0.0);
                        let contingency_np = contingency_ptdf
                            .and_then(|row| row.get(b).copied())
                            .unwrap_or(0.0);
                        let post_np = monitored_np + lodf_lk * contingency_np;
                        if post_np.abs() < 1e-12 {
                            continue;
                        }

                        let (headroom, limit) = if post_np > 0.0 {
                            let h = post_rating - post_flow;
                            (h, h / post_np)
                        } else {
                            let h = post_rating + post_flow;
                            (h, h / (-post_np))
                        };

                        if headroom < 0.0 {
                            *bus_limit = 0.0;
                            continue;
                        }
                        if limit < *bus_limit {
                            *bus_limit = limit;
                        }
                    }
                }
            }
        }

        let by_bus = self
            .network
            .buses
            .iter()
            .enumerate()
            .filter(|(idx, _)| distributed_slack || *idx != slack_idx)
            .map(|(idx, bus)| {
                (
                    bus.number,
                    trlim_by_bus[idx].max(0.0) * self.network.base_mva,
                )
            })
            .collect();

        Ok(InjectionCapabilityMap {
            by_bus,
            failed_contingencies,
        })
    }

    pub fn compute_afc(&mut self, request: &AfcRequest) -> Result<Vec<AfcResult>, TransferError> {
        self.validate_path(&request.path)?;
        let base_flows_mw: Vec<f64> = self
            .base_flows_pu
            .iter()
            .map(|&flow| flow * self.network.base_mva)
            .collect();

        let mut required_branches: Vec<usize> = request
            .flowgates
            .iter()
            .map(|fg| fg.monitored_branch)
            .collect();
        required_branches.extend(
            request
                .flowgates
                .iter()
                .filter_map(|fg| fg.contingency_branch),
        );
        required_branches.sort_unstable();
        required_branches.dedup();
        self.validate_branch_indices(&required_branches, "flowgate")?;

        for flowgate in &request.flowgates {
            if flowgate.normal_rating_mw <= 0.0 {
                return Err(TransferError::InvalidFlowgate {
                    name: flowgate.name.clone(),
                    reason: "normal_rating_mw must be positive".to_string(),
                });
            }
            if let Some(contingency_rating_mw) = flowgate.contingency_rating_mw
                && contingency_rating_mw <= 0.0
            {
                return Err(TransferError::InvalidFlowgate {
                    name: flowgate.name.clone(),
                    reason: "contingency_rating_mw must be positive when provided".to_string(),
                });
            }
            let monitored_branch = &self.network.branches[flowgate.monitored_branch];
            if !monitored_branch.in_service {
                return Err(TransferError::InvalidFlowgate {
                    name: flowgate.name.clone(),
                    reason: format!(
                        "monitored branch {} is out of service",
                        flowgate.monitored_branch
                    ),
                });
            }
            if let Some(contingency_branch) = flowgate.contingency_branch {
                if contingency_branch == flowgate.monitored_branch {
                    return Err(TransferError::InvalidFlowgate {
                        name: flowgate.name.clone(),
                        reason: "contingency branch must differ from monitored branch".to_string(),
                    });
                }
                let contingency = &self.network.branches[contingency_branch];
                if !contingency.in_service {
                    return Err(TransferError::InvalidFlowgate {
                        name: flowgate.name.clone(),
                        reason: format!(
                            "contingency branch {} is out of service",
                            contingency_branch
                        ),
                    });
                }
                if contingency.x.abs() < 1e-20 {
                    return Err(TransferError::InvalidFlowgate {
                        name: flowgate.name.clone(),
                        reason: format!(
                            "contingency branch {} is not a valid AC outage candidate",
                            contingency_branch
                        ),
                    });
                }
            }
        }

        let net_ptdf = self.interface_net_ptdf(&required_branches, &request.path)?;

        let mut outage_branches: Vec<usize> = request
            .flowgates
            .iter()
            .filter_map(|fg| fg.contingency_branch)
            .collect();
        outage_branches.sort_unstable();
        outage_branches.dedup();

        let mut lodf_monitored = required_branches.clone();
        lodf_monitored.extend_from_slice(&outage_branches);
        lodf_monitored.sort_unstable();
        lodf_monitored.dedup();

        let lodf_pairs = if outage_branches.is_empty() {
            None
        } else {
            Some(
                self.dc_model
                    .compute_lodf_pairs(&lodf_monitored, &outage_branches)?,
            )
        };

        request
            .flowgates
            .iter()
            .map(|fg| {
                let l = fg.monitored_branch;
                let base_flow_l = base_flows_mw.get(l).copied().unwrap_or(0.0);
                let base_np = net_ptdf.get(&l).copied().unwrap_or(0.0);

                let afc_for_state = |flow_mw: f64, net_ptdf_mw: f64, rating_mw: f64| -> f64 {
                    if net_ptdf_mw.abs() < 1e-12 {
                        f64::INFINITY
                    } else {
                        let headroom = if net_ptdf_mw > 0.0 {
                            rating_mw - flow_mw
                        } else {
                            rating_mw + flow_mw
                        };
                        (headroom / net_ptdf_mw.abs()).max(0.0)
                    }
                };

                let n0_afc = afc_for_state(base_flow_l, base_np, fg.normal_rating_mw);
                let (afc_mw, binding_ctg) = if let Some(k) = fg.contingency_branch {
                    let pairs =
                        lodf_pairs
                            .as_ref()
                            .ok_or_else(|| TransferError::InvalidFlowgate {
                                name: fg.name.clone(),
                                reason: "contingency branch requires LODF data".to_string(),
                            })?;
                    let diag = pairs
                        .get(k, k)
                        .ok_or_else(|| TransferError::InvalidFlowgate {
                            name: fg.name.clone(),
                            reason: format!("missing LODF diagonal for contingency branch {k}"),
                        })?;
                    if !diag.is_finite() {
                        return Err(TransferError::InvalidFlowgate {
                            name: fg.name.clone(),
                            reason: format!(
                                "contingency branch {k} is not usable for LODF-based AFC"
                            ),
                        });
                    }
                    let lodf_lk =
                        pairs
                            .get(l, k)
                            .ok_or_else(|| TransferError::InvalidFlowgate {
                                name: fg.name.clone(),
                                reason: format!(
                                    "missing LODF entry for monitored branch {l} and outage {k}"
                                ),
                            })?;
                    let post_flow =
                        base_flow_l + lodf_lk * base_flows_mw.get(k).copied().unwrap_or(0.0);
                    let post_np = net_ptdf.get(&l).copied().unwrap_or(0.0)
                        + lodf_lk * net_ptdf.get(&k).copied().unwrap_or(0.0);
                    let n1_afc =
                        afc_for_state(post_flow, post_np, fg.effective_contingency_rating_mw());
                    if n1_afc <= n0_afc {
                        (n1_afc, Some(k))
                    } else {
                        (n0_afc, None)
                    }
                } else {
                    (n0_afc, None)
                };

                Ok(AfcResult {
                    flowgate_name: fg.name.clone(),
                    afc_mw,
                    binding_branch: l,
                    binding_contingency: binding_ctg,
                })
            })
            .collect()
    }

    pub fn compute_multi_transfer(
        &mut self,
        request: &MultiTransferRequest,
    ) -> Result<MultiTransferResult, TransferError> {
        let n_iface = request.paths.len();
        if n_iface == 0 {
            return Err(TransferError::InvalidRequest(
                "at least one transfer path is required".to_string(),
            ));
        }
        for path in &request.paths {
            self.validate_path(path)?;
        }

        let weights = request.weights.as_deref().unwrap_or(&[]);
        if !weights.is_empty() && weights.len() != n_iface {
            return Err(TransferError::InvalidRequest(format!(
                "weights length {} does not match path count {n_iface}",
                weights.len()
            )));
        }
        let max_transfer_mw = request.max_transfer_mw.as_deref().unwrap_or(&[]);
        if !max_transfer_mw.is_empty() && max_transfer_mw.len() != n_iface {
            return Err(TransferError::InvalidRequest(format!(
                "max_transfer_mw length {} does not match path count {n_iface}",
                max_transfer_mw.len()
            )));
        }
        if max_transfer_mw.iter().any(|value| *value <= 0.0) {
            return Err(TransferError::InvalidRequest(
                "max_transfer_mw values must all be positive".to_string(),
            ));
        }

        let bus_map = self.network.bus_index_map();
        let branch_ratings_pu = self.branch_ratings_pu();
        let active_branches: Vec<usize> = self
            .network
            .branches
            .iter()
            .enumerate()
            .filter_map(|(branch_idx, branch)| {
                let rating = branch_ratings_pu[branch_idx];
                (branch.in_service && rating.is_finite() && rating > 1e-10).then_some(branch_idx)
            })
            .collect();
        let interface_bus_indices = collect_interface_bus_indices(
            &bus_map,
            request
                .paths
                .iter()
                .flat_map(|path| path.source_buses.iter().chain(path.sink_buses.iter())),
        );
        let ptdf = self
            .dc_model
            .compute_ptdf_request(
                &PtdfRequest::for_branches(&active_branches)
                    .with_bus_indices(&interface_bus_indices),
            )
            .map_err(TransferError::from)?;

        let mut net_ptdf_matrix: Vec<Vec<f64>> = Vec::with_capacity(n_iface);
        for path in &request.paths {
            let n_from = path.source_buses.len() as f64;
            let n_to = path.sink_buses.len() as f64;
            let mut net_ptdf = vec![0.0f64; active_branches.len()];
            for (branch_pos, &branch_idx) in active_branches.iter().enumerate() {
                let from_sum: f64 = path
                    .source_buses
                    .iter()
                    .filter_map(|bus| bus_map.get(bus))
                    .map(|&bus_idx| ptdf.get(branch_idx, bus_idx))
                    .sum();
                let to_sum: f64 = path
                    .sink_buses
                    .iter()
                    .filter_map(|bus| bus_map.get(bus))
                    .map(|&bus_idx| ptdf.get(branch_idx, bus_idx))
                    .sum();
                net_ptdf[branch_pos] = from_sum / n_from - to_sum / n_to;
            }
            net_ptdf_matrix.push(net_ptdf);
        }

        crate::multi_transfer::solve_multi_interface_transfer_lp(
            self.network.base_mva,
            &active_branches,
            &self.base_flows_pu,
            &branch_ratings_pu,
            &net_ptdf_matrix,
            weights,
            max_transfer_mw,
        )
    }

    pub fn compute_nerc_atc(
        &mut self,
        request: &NercAtcRequest,
    ) -> Result<NercAtcResult, TransferError> {
        self.validate_path(&request.path)?;

        let margins = request.options.margins;
        let n_br = self.network.n_branches();
        let base_mva = self.network.base_mva;
        let monitored_branches = request
            .options
            .monitored_branches
            .clone()
            .unwrap_or_else(|| self.default_monitored_branches());
        self.validate_branch_indices(&monitored_branches, "monitored")?;
        let contingency_branches = request
            .options
            .contingency_branches
            .clone()
            .unwrap_or_default();
        self.validate_branch_indices(&contingency_branches, "contingency")?;

        let mut required_branches = monitored_branches.clone();
        required_branches.extend_from_slice(&contingency_branches);
        required_branches.sort_unstable();
        required_branches.dedup();

        let net_ptdf = self.interface_net_ptdf(&required_branches, &request.path)?;
        let transfer_ptdf: Vec<f64> = monitored_branches
            .iter()
            .map(|branch_idx| net_ptdf.get(branch_idx).copied().unwrap_or(0.0))
            .collect();

        let headroom_for = |flow_mw: f64, net_ptdf: f64, rating_mw: f64| -> f64 {
            if net_ptdf > 0.0 {
                (rating_mw - flow_mw) / net_ptdf
            } else {
                (rating_mw + flow_mw) / (-net_ptdf)
            }
        };

        let mut ttc_mw = f64::INFINITY;
        let mut limit_cause = NercAtcLimitCause::Unconstrained;

        for (position, &branch_idx) in monitored_branches.iter().enumerate() {
            let branch = &self.network.branches[branch_idx];
            if !branch.in_service || branch.rating_a_mva <= 0.0 {
                continue;
            }

            let net_ptdf = transfer_ptdf[position];
            if net_ptdf.abs() < 1e-12 {
                continue;
            }

            let flow_mw = self.base_flows_pu[branch_idx] * base_mva;
            let headroom_mw = headroom_for(flow_mw, net_ptdf, branch.rating_a_mva).max(0.0);
            if headroom_mw < ttc_mw {
                ttc_mw = headroom_mw;
                limit_cause = NercAtcLimitCause::BasecaseThermal {
                    monitored_branch: branch_idx,
                };
            }
        }

        if ttc_mw > 0.0 && !contingency_branches.is_empty() {
            let active_ctg_branches: Vec<usize> = contingency_branches
                .iter()
                .copied()
                .filter(|&branch_idx| {
                    branch_idx < n_br && self.network.branches[branch_idx].in_service
                })
                .collect();

            if !active_ctg_branches.is_empty() {
                let fail_closed_for_outage =
                    |k: usize, ttc_mw: &mut f64, limit_cause: &mut NercAtcLimitCause| {
                        warn!(
                            contingency_branch = k,
                            "non-finite outage sensitivity; treating ATC as zero"
                        );
                        *ttc_mw = 0.0;
                        *limit_cause = NercAtcLimitCause::FailClosedOutage {
                            contingency_branch: k,
                        };
                    };
                let lodf = self
                    .dc_model
                    .compute_lodf(&monitored_branches, &active_ctg_branches)?;
                let contingency_transfer_ptdf: Vec<f64> = active_ctg_branches
                    .iter()
                    .map(|branch_idx| net_ptdf.get(branch_idx).copied().unwrap_or(0.0))
                    .collect();

                for (ctg_position, &ctg_branch) in active_ctg_branches.iter().enumerate() {
                    for (position, &branch_idx) in monitored_branches.iter().enumerate() {
                        if branch_idx == ctg_branch || !self.network.branches[branch_idx].in_service
                        {
                            continue;
                        }

                        let rating_mw = self.network.branches[branch_idx].rating_a_mva;
                        if rating_mw <= 0.0 {
                            continue;
                        }

                        let lodf_lk = lodf[(position, ctg_position)];
                        if !lodf_lk.is_finite() {
                            fail_closed_for_outage(ctg_branch, &mut ttc_mw, &mut limit_cause);
                            break;
                        }

                        let post_flow_mw = (self.base_flows_pu[branch_idx]
                            + lodf_lk * self.base_flows_pu[ctg_branch])
                            * base_mva;
                        let post_net_ptdf = transfer_ptdf[position]
                            + lodf_lk * contingency_transfer_ptdf[ctg_position];
                        if post_net_ptdf.abs() < 1e-12 {
                            continue;
                        }

                        let headroom_mw =
                            headroom_for(post_flow_mw, post_net_ptdf, rating_mw).max(0.0);
                        if headroom_mw < ttc_mw {
                            ttc_mw = headroom_mw;
                            limit_cause = NercAtcLimitCause::ContingencyThermal {
                                monitored_branch: branch_idx,
                                contingency_branch: ctg_branch,
                            };
                        }

                        if ttc_mw == 0.0 {
                            break;
                        }
                    }

                    if ttc_mw == 0.0 {
                        break;
                    }
                }
            }
        }

        let adjacent_buses: HashSet<u32> = {
            let mut buses = HashSet::new();
            for &bus in request
                .path
                .source_buses
                .iter()
                .chain(request.path.sink_buses.iter())
            {
                buses.insert(bus);
            }
            for branch in &self.network.branches {
                if !branch.in_service {
                    continue;
                }
                if buses.contains(&branch.from_bus) || buses.contains(&branch.to_bus) {
                    buses.insert(branch.from_bus);
                    buses.insert(branch.to_bus);
                }
            }
            buses
        };

        let reactive_margin_warning = self.network.generators.iter().any(|generator| {
            generator.in_service
                && adjacent_buses.contains(&generator.bus)
                && generator.qmax > 0.0
                && generator.q.abs() > 0.70 * generator.qmax
        });

        let trm_mw = if ttc_mw.is_infinite() {
            0.0
        } else {
            margins.trm_fraction * ttc_mw
        };
        let atc_mw = (ttc_mw - trm_mw - margins.cbm_mw - margins.etc_mw).max(0.0);

        Ok(NercAtcResult {
            atc_mw,
            ttc_mw,
            trm_mw,
            cbm_mw: margins.cbm_mw,
            etc_mw: margins.etc_mw,
            limit_cause,
            monitored_branches,
            transfer_ptdf,
            reactive_margin_warning,
        })
    }
}

fn collect_interface_bus_indices<'a, I>(bus_map: &HashMap<u32, usize>, buses: I) -> Vec<usize>
where
    I: IntoIterator<Item = &'a u32>,
{
    let mut bus_indices = Vec::new();
    for bus in buses {
        if let Some(&bus_idx) = bus_map.get(bus)
            && !bus_indices.contains(&bus_idx)
        {
            bus_indices.push(bus_idx);
        }
    }
    bus_indices
}

/// Compute Available Flowgate Capability for a list of `Flowgate`s.
///
/// For each flowgate, the AFC is the headroom on the monitored corridor
/// considering both the N-0 condition and (if `flowgate.contingency_branch`
/// is set) the post-contingency N-1 condition.
///
/// The interface net PTDF is used to convert MW headroom on the monitored
/// branch into an equivalent transfer capability at the interface.
///
/// # Returns
/// One [`AfcResult`] per element of `request.flowgates`.
pub fn compute_afc(
    network: &Network,
    request: &AfcRequest,
) -> Result<Vec<AfcResult>, TransferError> {
    info!(
        flowgates = request.flowgates.len(),
        interface = %request.path.name,
        "computing AFC for flowgates"
    );
    PreparedTransferModel::new(network)?.compute_afc(request)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::injection::{
        InjectionCapabilityMap, InjectionCapabilityOptions, compute_injection_capability,
    };
    use crate::matrices::{compute_bldf, compute_gsf};
    use surge_dc::DcSensitivityOptions;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    fn inj_cap(net: &Network, post_ctg_frac: f64) -> Result<InjectionCapabilityMap, TransferError> {
        compute_injection_capability(
            net,
            &InjectionCapabilityOptions {
                post_contingency_rating_fraction: post_ctg_frac,
                ..InjectionCapabilityOptions::default()
            },
        )
    }

    fn radial_bridge_network() -> Network {
        let mut net = Network::new("radial_bridge");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PV, 230.0));
        net.buses.push(Bus::new(3, BusType::PQ, 230.0));

        let mut br12 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br12.rating_a_mva = 100.0;
        net.branches.push(br12);

        let mut br23 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br23.rating_a_mva = 100.0;
        net.branches.push(br23);

        net.generators.push(Generator::new(1, 50.0, 1.0));
        net.generators.push(Generator::new(2, 0.0, 1.0));
        net.loads.push(Load::new(3, 50.0, 0.0));
        net
    }
    use crate::test_util::case_path;
    use crate::types::{AfcRequest, Flowgate, NercAtcLimitCause, TransferPath};
    use serde::{Deserialize, Serialize};
    use surge_dc::{LodfMatrixRequest, PtdfRequest, compute_lodf_matrix, compute_ptdf, solve_dc};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TransferCapabilityResult {
        pub atc_mw: f64,
        pub ttc_mw: f64,
        pub limit_cause: NercAtcLimitCause,
    }

    fn compute_transfer_capability_from_sensitivities(
        network: &Network,
        ptdf: &PtdfRows,
        lodf: &surge_dc::LodfMatrixResult,
        base_flows: &[f64],
        ratings: &[f64],
        interface: &TransferPath,
        trm_fraction: f64,
    ) -> TransferCapabilityResult {
        let bus_map = network.bus_index_map();
        let n_br = network.n_branches();

        info!(
            interface = %interface.name,
            source_buses = interface.source_buses.len(),
            sink_buses = interface.sink_buses.len(),
            trm_fraction = trm_fraction,
            "computing ATC"
        );

        debug_assert_eq!(base_flows.len(), n_br);
        debug_assert_eq!(ratings.len(), n_br);

        let n_from = interface.source_buses.len().max(1) as f64;
        let n_to = interface.sink_buses.len().max(1) as f64;

        let mut net_ptdf = vec![0.0f64; n_br];
        for (l, slot) in net_ptdf.iter_mut().enumerate() {
            let from_sum: f64 = interface
                .source_buses
                .iter()
                .filter_map(|b| bus_map.get(b))
                .map(|&idx| ptdf.get(l, idx))
                .sum();
            let to_sum: f64 = interface
                .sink_buses
                .iter()
                .filter_map(|b| bus_map.get(b))
                .map(|&idx| ptdf.get(l, idx))
                .sum();
            *slot = from_sum / n_from - to_sum / n_to;
        }

        let mut ttc = f64::INFINITY;
        let mut limit_cause = NercAtcLimitCause::Unconstrained;

        let try_limit = |limit: f64,
                         br: usize,
                         ctg: Option<usize>,
                         ttc: &mut f64,
                         cause: &mut NercAtcLimitCause| {
            let clamped = limit.max(0.0);
            if clamped < *ttc {
                *ttc = clamped;
                *cause = match ctg {
                    Some(contingency_branch) => NercAtcLimitCause::ContingencyThermal {
                        monitored_branch: br,
                        contingency_branch,
                    },
                    None => NercAtcLimitCause::BasecaseThermal {
                        monitored_branch: br,
                    },
                };
            }
        };

        for l in 0..n_br {
            if !network.branches[l].in_service {
                continue;
            }
            let np = net_ptdf[l];
            if np.abs() < 1e-12 {
                continue;
            }
            let limit = if np > 0.0 {
                (ratings[l] - base_flows[l]) / np
            } else {
                (ratings[l] + base_flows[l]) / (-np)
            };
            if limit < 0.0 {
                ttc = 0.0;
                limit_cause = NercAtcLimitCause::BasecaseThermal {
                    monitored_branch: l,
                };
                break;
            }
            try_limit(limit, l, None, &mut ttc, &mut limit_cause);
        }

        if ttc > 0.0 {
            for k in 0..n_br {
                if !network.branches[k].in_service {
                    continue;
                }
                if !lodf[(k, k)].is_finite() {
                    ttc = 0.0;
                    limit_cause = NercAtcLimitCause::FailClosedOutage {
                        contingency_branch: k,
                    };
                    break;
                }

                let lodf_row_k: Vec<f64> = (0..n_br).map(|l| lodf[(l, k)]).collect();

                for l in 0..n_br {
                    if l == k {
                        continue;
                    }
                    if !network.branches[l].in_service {
                        continue;
                    }
                    let lodf_lk = lodf_row_k[l];
                    if !lodf_lk.is_finite() {
                        ttc = 0.0;
                        limit_cause = NercAtcLimitCause::ContingencyThermal {
                            monitored_branch: l,
                            contingency_branch: k,
                        };
                        break;
                    }

                    let post_flow = base_flows[l] + lodf_lk * base_flows[k];
                    let post_np = net_ptdf[l] + lodf_lk * net_ptdf[k];

                    if post_np.abs() < 1e-12 {
                        continue;
                    }
                    let limit = if post_np > 0.0 {
                        (ratings[l] - post_flow) / post_np
                    } else {
                        (ratings[l] + post_flow) / (-post_np)
                    };
                    if limit < 0.0 {
                        ttc = 0.0;
                        limit_cause = NercAtcLimitCause::ContingencyThermal {
                            monitored_branch: l,
                            contingency_branch: k,
                        };
                        break;
                    }
                    try_limit(limit, l, Some(k), &mut ttc, &mut limit_cause);
                }

                if ttc == 0.0 {
                    break;
                }
            }
        }

        let ttc_mw = if ttc.is_infinite() {
            f64::INFINITY
        } else {
            ttc
        };
        let atc_mw = (ttc_mw * (1.0 - trm_fraction)).max(0.0);

        if ttc_mw == 0.0 {
            warn!(
                interface = %interface.name,
                limit_cause = %limit_cause,
                "ATC is zero — network already overloaded or at thermal limit"
            );
        }

        info!(
            interface = %interface.name,
            atc_mw = atc_mw,
            ttc_mw = ttc_mw,
            limit_cause = %limit_cause,
            "ATC computed"
        );

        TransferCapabilityResult {
            atc_mw,
            ttc_mw,
            limit_cause,
        }
    }

    fn load_case9() -> Network {
        surge_io::load(case_path("case9")).expect("failed to parse case9")
    }

    fn transfer_path(name: &str, source_buses: Vec<u32>, sink_buses: Vec<u32>) -> TransferPath {
        TransferPath::new(name, source_buses, sink_buses)
    }

    // ── GSF ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_gsf_case9() {
        let net = load_case9();
        let n_br = net.n_branches();
        let all_branches: Vec<usize> = (0..n_br).collect();
        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all_branches)).unwrap();
        let gsf = compute_gsf(&net).unwrap();

        let n_gen = net.generators.iter().filter(|g| g.in_service).count();

        // Dimension check.
        assert_eq!(gsf.values.nrows(), n_br, "GSF rows != n_branches");
        assert_eq!(
            gsf.values.ncols(),
            n_gen,
            "GSF cols != n_in_service_generators"
        );
        assert_eq!(gsf.gen_buses.len(), n_gen);
        assert_eq!(gsf.branch_ids.len(), n_br);
        assert_eq!(gsf.branch_ids, (0..n_br).collect::<Vec<_>>());

        // Physical sanity: |GSF| ≤ 1 for lossless DC networks.
        for l in 0..n_br {
            for g in 0..n_gen {
                let v = gsf.values[(l, g)];
                assert!(
                    v.abs() <= 1.0 + 1e-10,
                    "GSF[{l},{g}] = {v} exceeds 1.0 (violates DC power flow physics)"
                );
            }
        }

        // Canonical layout: GSF[l,g] must equal PTDF[l, bus_idx_for_gen_g].
        let bus_map = net.bus_index_map();
        for (g, g_obj) in net.generators.iter().filter(|g| g.in_service).enumerate() {
            if let Some(&b_idx) = bus_map.get(&g_obj.bus) {
                for l in 0..n_br {
                    let expected = ptdf.get(l, b_idx);
                    let got = gsf.values[(l, g)];
                    assert!(
                        (got - expected).abs() < 1e-12,
                        "GSF[{l},{g}] = {got:.6e}, expected PTDF[{l},{b_idx}] = {expected:.6e}"
                    );
                }
            }
        }
    }

    // ── BLDF ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_bldf_case9() {
        let net = load_case9();
        let n_br = net.n_branches();
        let all_branches: Vec<usize> = (0..n_br).collect();
        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all_branches)).unwrap();
        let bldf = compute_bldf(&net).unwrap();

        let n_bus = net.n_buses();

        assert_eq!(bldf.values.nrows(), n_bus, "BLDF rows != n_buses");
        assert_eq!(bldf.values.ncols(), n_br, "BLDF cols != n_branches");

        // BLDF[slack, l] must be 0 for all l (PTDF slack column = 0 → BLDF = 0).
        let slack_idx = net.slack_bus_index().unwrap();
        for l in 0..n_br {
            let v = bldf.values[(slack_idx, l)];
            assert!(
                v.abs() < 1e-10,
                "BLDF[slack={slack_idx},{l}] = {v:.6e}, expected 0"
            );
        }

        // BLDF must be the exact negation of PTDF rows.
        for b in 0..n_bus {
            for l in 0..n_br {
                let expected = -ptdf.get(l, b);
                let got = bldf.values[(b, l)];
                assert!(
                    (got - expected).abs() < 1e-12,
                    "BLDF[{b},{l}] = {got:.6e}, expected {expected:.6e}"
                );
            }
        }
    }

    // ── ATC ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_atc_case9() {
        let net = load_case9();
        let n_br = net.n_branches();
        let all_branches: Vec<usize> = (0..n_br).collect();
        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all_branches)).unwrap();
        let lodf =
            compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all_branches)).unwrap();
        let dc = solve_dc(&net).expect("DC PF should converge");

        // Use rate_a from the network; if zero, assign a generous default.
        let ratings: Vec<f64> = net
            .branches
            .iter()
            .map(|br| {
                if br.rating_a_mva > 1e-6 {
                    br.rating_a_mva / net.base_mva
                } else {
                    2.5
                }
            })
            .collect();

        // Transfer from bus 2 (non-slack generator bus) to bus 9 (load bus).
        // Avoid using the slack bus (bus 1) as from_bus: PTDF[slack]=0 for all
        // branches, which would make net_ptdf all zeros and yield TTC=∞ (no
        // binding constraint found), which is a degenerate result.
        let iface = transfer_path("bus2_to_bus9", vec![2], vec![9]);

        let result = compute_transfer_capability_from_sensitivities(
            &net,
            &ptdf,
            &lodf,
            &dc.branch_p_flow,
            &ratings,
            &iface,
            0.0, // no TRM
        );

        // ATC must be non-negative.
        assert!(
            result.atc_mw >= 0.0,
            "ATC should be non-negative, got {}",
            result.atc_mw
        );

        // Must be bounded — there are finite ratings (non-slack from_bus).
        assert!(
            result.ttc_mw.is_finite(),
            "TTC should be finite (bounded by thermal ratings), got {}",
            result.ttc_mw
        );

        // With no TRM, ATC == TTC.
        assert!(
            (result.atc_mw - result.ttc_mw).abs() < 1e-10,
            "With trm=0, ATC ({}) must equal TTC ({})",
            result.atc_mw,
            result.ttc_mw
        );

        // Positive finite TTC should identify a monitored binding branch.
        if result.ttc_mw.is_finite() && result.ttc_mw > 0.0 && result.ttc_mw < 1e15 {
            assert!(
                result.limit_cause.monitored_branch().is_some(),
                "Binding branch should be identified when TTC is finite"
            );
        }

        // TTC is either positive, or fail-closed to zero when an N-1 outage
        // islands the case / invalidates the linear outage model.
        if result.ttc_mw == 0.0 {
            assert!(
                result.limit_cause.contingency_branch().is_some(),
                "zero TTC should be tied to an explicit binding contingency"
            );
        } else {
            assert!(
                result.ttc_mw > 0.0,
                "TTC from non-slack bus should be positive when sensitivities stay finite, got {}",
                result.ttc_mw
            );
        }
    }

    #[test]
    fn test_interface_net_ptdf_matches_full_ptdf() {
        let net = load_case9();
        let monitored = vec![0usize, 2usize, 5usize];
        let interface = transfer_path("bus2_to_bus9", vec![2], vec![9]);
        let bus_map = net.bus_index_map();
        let source_idx = *bus_map.get(&2).expect("source bus");
        let sink_idx = *bus_map.get(&9).expect("sink bus");

        let mut prepared = PreparedTransferModel::new(&net).expect("prepared transfer model");
        let net_ptdf = prepared
            .interface_net_ptdf(&monitored, &interface)
            .expect("interface net PTDF");

        let full_ptdf =
            compute_ptdf(&net, &PtdfRequest::for_branches(&monitored)).expect("full PTDF");
        for &branch_idx in &monitored {
            let expected =
                full_ptdf.get(branch_idx, source_idx) - full_ptdf.get(branch_idx, sink_idx);
            let got = net_ptdf.get(&branch_idx).copied().unwrap_or(0.0);
            assert!(
                (got - expected).abs() < 1e-12,
                "branch {branch_idx}: interface net PTDF mismatch"
            );
        }
    }

    /// Multi-bus interface: verify equal-share normalization.
    ///
    /// When using a 2-bus from-interface {bus2, bus3} and a 2-bus to-interface
    /// {bus7, bus8}, the net PTDF should equal the average PTDF difference.
    /// ATC for the 2-bus interface must equal ATC for the equivalent single-bus
    /// interface at the centroid — verified by checking both return finite, positive values.
    #[test]
    fn test_atc_multi_bus_interface() {
        let net = load_case9();
        let n_br = net.n_branches();
        let all_branches: Vec<usize> = (0..n_br).collect();
        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all_branches)).unwrap();
        let lodf =
            compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all_branches)).unwrap();
        let dc = solve_dc(&net).expect("DC PF");

        let ratings: Vec<f64> = net
            .branches
            .iter()
            .map(|br| {
                if br.rating_a_mva > 1e-6 {
                    br.rating_a_mva / net.base_mva
                } else {
                    2.5
                }
            })
            .collect();

        // Single-bus interface
        let single = transfer_path("bus2_to_bus7", vec![2], vec![7]);

        // Multi-bus interface — buses 2+3 on from-side, buses 7+8 on to-side.
        let multi = transfer_path("buses23_to_78", vec![2, 3], vec![7, 8]);

        let atc_single = compute_transfer_capability_from_sensitivities(
            &net,
            &ptdf,
            &lodf,
            &dc.branch_p_flow,
            &ratings,
            &single,
            0.0,
        );
        let atc_multi = compute_transfer_capability_from_sensitivities(
            &net,
            &ptdf,
            &lodf,
            &dc.branch_p_flow,
            &ratings,
            &multi,
            0.0,
        );

        // Both should produce finite non-negative ATC.
        assert!(
            atc_single.ttc_mw.is_finite() && atc_single.ttc_mw >= 0.0,
            "Single-bus ATC should be finite non-negative, got {}",
            atc_single.ttc_mw
        );
        assert!(
            atc_multi.ttc_mw.is_finite() && atc_multi.ttc_mw >= 0.0,
            "Multi-bus ATC should be finite non-negative, got {}",
            atc_multi.ttc_mw
        );

        // Multi-bus TTC should differ from single-bus TTC (different interfaces).
        // Both should be physically reasonable (between 0 and a generous upper bound).
        assert!(
            atc_multi.ttc_mw < 100.0, // 100 pu = 10000 MW on 100 MVA base — very generous
            "Multi-bus ATC suspiciously large: {}",
            atc_multi.ttc_mw
        );
    }

    #[test]
    fn test_atc_with_trm() {
        let net = load_case9();
        let n_br = net.n_branches();
        let all_branches: Vec<usize> = (0..n_br).collect();
        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&all_branches)).unwrap();
        let lodf =
            compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all_branches)).unwrap();
        let dc = solve_dc(&net).expect("DC PF");

        let ratings: Vec<f64> = net
            .branches
            .iter()
            .map(|br| {
                if br.rating_a_mva > 1e-6 {
                    br.rating_a_mva / net.base_mva
                } else {
                    2.5
                }
            })
            .collect();

        let iface = transfer_path("bus2_to_bus7", vec![2], vec![7]);

        let no_trm = compute_transfer_capability_from_sensitivities(
            &net,
            &ptdf,
            &lodf,
            &dc.branch_p_flow,
            &ratings,
            &iface,
            0.0,
        );
        let with_trm = compute_transfer_capability_from_sensitivities(
            &net,
            &ptdf,
            &lodf,
            &dc.branch_p_flow,
            &ratings,
            &iface,
            0.10,
        );

        // ATC with TRM must be ≤ ATC without TRM.
        assert!(
            with_trm.atc_mw <= no_trm.atc_mw + 1e-10,
            "ATC with TRM ({}) should be ≤ ATC without TRM ({})",
            with_trm.atc_mw,
            no_trm.atc_mw
        );

        // TTC is unchanged by TRM.
        assert!(
            (with_trm.ttc_mw - no_trm.ttc_mw).abs() < 1e-10,
            "TTC should not change with TRM"
        );
    }

    // ── Injection capability ──────────────────────────────────────────────────

    #[test]
    fn test_injection_capability_case9() {
        let net = load_case9();
        let cap = inj_cap(&net, 1.0).unwrap();

        // case9 has 9 buses; slack excluded → 8 entries.
        assert_eq!(
            cap.by_bus.len(),
            net.n_buses() - 1,
            "Should have one entry per non-slack bus"
        );

        // All capabilities must be non-negative (base case is not overloaded).
        for &(bus_num, cap_mw) in &cap.by_bus {
            assert!(
                cap_mw >= 0.0,
                "Injection capability at bus {bus_num} is negative ({cap_mw})"
            );
        }

        // Sanity: at least one bus has a finite positive limit.
        let has_finite = cap.by_bus.iter().any(|&(_, v)| v.is_finite() && v > 0.0);
        assert!(
            has_finite,
            "At least one bus should have a finite capability"
        );
    }

    #[test]
    fn test_injection_capability_rejects_non_finite_rating() {
        let net = load_case9();
        let nan_result = compute_injection_capability(
            &net,
            &InjectionCapabilityOptions {
                post_contingency_rating_fraction: f64::NAN,
                ..InjectionCapabilityOptions::default()
            },
        );
        assert!(
            nan_result.is_err(),
            "NaN rating fraction should be rejected"
        );

        let inf_result = compute_injection_capability(
            &net,
            &InjectionCapabilityOptions {
                post_contingency_rating_fraction: f64::INFINITY,
                ..InjectionCapabilityOptions::default()
            },
        );
        assert!(
            inf_result.is_err(),
            "Infinity rating fraction should be rejected"
        );

        let neg_result = compute_injection_capability(
            &net,
            &InjectionCapabilityOptions {
                post_contingency_rating_fraction: -1.0,
                ..InjectionCapabilityOptions::default()
            },
        );
        assert!(
            neg_result.is_err(),
            "Negative rating fraction should be rejected"
        );

        let zero_result = compute_injection_capability(
            &net,
            &InjectionCapabilityOptions {
                post_contingency_rating_fraction: 0.0,
                ..InjectionCapabilityOptions::default()
            },
        );
        assert!(
            zero_result.is_err(),
            "Zero rating fraction should be rejected"
        );
    }

    #[test]
    fn test_injection_capability_distributed_slack() {
        let net = load_case9();
        let bus_map = net.bus_index_map();

        // Build participation factor weights from two generator buses.
        let weights: Vec<(usize, f64)> = net
            .generators
            .iter()
            .filter(|g| g.in_service)
            .filter_map(|g| bus_map.get(&g.bus).map(|&idx| (idx, 1.0)))
            .collect();
        assert!(
            !weights.is_empty(),
            "Need generators for distributed slack test"
        );

        let sens_opts = surge_dc::DcSensitivityOptions::with_slack_weights(&weights);

        let cap_distributed = compute_injection_capability(
            &net,
            &InjectionCapabilityOptions {
                sensitivity_options: Some(sens_opts),
                ..InjectionCapabilityOptions::default()
            },
        )
        .expect("distributed slack injection capability should succeed");

        let cap_single = inj_cap(&net, 1.0).expect("single slack capability");

        // With distributed slack, all buses (including former slack) should be
        // in the result.
        assert_eq!(
            cap_distributed.by_bus.len(),
            net.n_buses(),
            "Distributed slack should include all buses"
        );
        assert_eq!(
            cap_single.by_bus.len(),
            net.n_buses() - 1,
            "Single slack should exclude slack bus"
        );

        let slack_bus = net.buses[net.slack_bus_index().expect("slack bus")].number;
        let slack_cap = cap_distributed
            .by_bus
            .iter()
            .find(|(bus, _)| *bus == slack_bus)
            .map(|(_, cap)| *cap)
            .expect("distributed-slack capability should include the slack bus");
        assert!(
            slack_cap.is_finite(),
            "distributed-slack capability should compute a finite slack-bus limit"
        );

        // All capabilities should be non-negative.
        for &(bus_num, cap_mw) in &cap_distributed.by_bus {
            assert!(
                cap_mw >= 0.0,
                "Distributed slack capability at bus {bus_num} is negative ({cap_mw})"
            );
        }
    }

    #[test]
    fn test_injection_capability_exact_distributed_slack_includes_reference_bus() {
        let net = radial_bridge_network();
        let bus_map = net.bus_index_map();
        let weights: Vec<(usize, f64)> = net
            .generators
            .iter()
            .filter(|g| g.in_service)
            .filter_map(|g| bus_map.get(&g.bus).map(|&idx| (idx, 1.0)))
            .collect();
        let sens_opts = DcSensitivityOptions::with_slack_weights(&weights);
        let cap = compute_injection_capability(
            &net,
            &InjectionCapabilityOptions {
                exact: true,
                sensitivity_options: Some(sens_opts),
                ..InjectionCapabilityOptions::default()
            },
        )
        .expect("exact distributed slack injection capability should succeed");

        let slack_bus = net.buses[net.slack_bus_index().expect("slack bus")].number;
        let slack_cap = cap
            .by_bus
            .iter()
            .find(|(bus, _)| *bus == slack_bus)
            .map(|(_, cap)| *cap)
            .expect("slack bus should be present when distributed slack is enabled");
        assert!(
            slack_cap.is_finite(),
            "exact distributed-slack capability should not skip the reference bus"
        );
    }

    #[test]
    fn test_injection_capability_bridge_outage_falls_back_to_exact() {
        let net = radial_bridge_network();
        let approx = compute_injection_capability(
            &net,
            &InjectionCapabilityOptions {
                exact: false,
                ..InjectionCapabilityOptions::default()
            },
        )
        .expect("approximate injection capability should succeed");
        let exact = compute_injection_capability(
            &net,
            &InjectionCapabilityOptions {
                exact: true,
                ..InjectionCapabilityOptions::default()
            },
        )
        .expect("exact injection capability should succeed");

        assert_eq!(approx.by_bus.len(), exact.by_bus.len());
        for ((approx_bus, approx_cap), (exact_bus, exact_cap)) in
            approx.by_bus.iter().zip(exact.by_bus.iter())
        {
            assert_eq!(approx_bus, exact_bus);
            assert!(
                *approx_cap <= *exact_cap + 1e-9,
                "approximate capability should not exceed exact fallback result for bus {approx_bus}"
            );
        }
    }

    #[test]
    fn test_injection_capability_emergency_rating() {
        let net = load_case9();
        let normal = inj_cap(&net, 1.0).unwrap();
        let emergency = inj_cap(&net, 1.25).unwrap();

        // Emergency rating (125%) yields ≥ normal-rating capability for every bus.
        for ((_, n_cap), (bus, e_cap)) in normal.by_bus.iter().zip(emergency.by_bus.iter()) {
            assert!(
                *e_cap >= *n_cap - 1e-10,
                "Emergency capability at bus {bus} ({e_cap}) < normal ({n_cap})"
            );
        }
    }

    // ── AFC ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_afc_case9() {
        let net = load_case9();
        let mut model = PreparedTransferModel::new(&net).expect("case9 transfer model");
        let rating_mw: Vec<f64> = net
            .branches
            .iter()
            .map(|br| {
                if br.rating_a_mva > 1e-6 {
                    br.rating_a_mva
                } else {
                    250.0
                }
            })
            .collect();

        let iface = transfer_path("bus1_to_bus9", vec![1], vec![9]);

        // N-0 flowgate on branch 0, with the rating from the flowgate (not the branch array).
        let flowgates = vec![
            Flowgate::new("FG_branch0_N0", 0, None, rating_mw[0], None),
            Flowgate::new("FG_branch1_N1_ctg2", 1, Some(2), rating_mw[1], None),
        ];

        let request = AfcRequest {
            path: iface,
            flowgates: flowgates.clone(),
        };
        let results = model.compute_afc(&request).unwrap();

        assert_eq!(
            results.len(),
            flowgates.len(),
            "One AFC result per flowgate"
        );

        for (fg, res) in flowgates.iter().zip(results.iter()) {
            assert_eq!(res.flowgate_name, fg.name);
            assert!(
                res.afc_mw >= 0.0,
                "AFC for '{}' is negative ({})",
                fg.name,
                res.afc_mw
            );
        }

        let branch_idx = flowgates[1].monitored_branch;
        let contingency_idx = flowgates[1]
            .contingency_branch
            .expect("test flowgate should be N-1");
        let afc_for_state = |flow_mw: f64, net_ptdf_mw: f64, rating_mw: f64| -> f64 {
            if net_ptdf_mw.abs() < 1e-12 {
                f64::INFINITY
            } else {
                let headroom = if net_ptdf_mw > 0.0 {
                    rating_mw - flow_mw
                } else {
                    rating_mw + flow_mw
                };
                (headroom / net_ptdf_mw.abs()).max(0.0)
            }
        };
        let transfer_ptdf = model
            .interface_net_ptdf(&[branch_idx, contingency_idx], &request.path)
            .expect("transfer PTDF");
        let base_flow = model.base_flows_pu[branch_idx] * net.base_mva;
        let base_np = transfer_ptdf
            .get(&branch_idx)
            .copied()
            .expect("monitored branch PTDF");
        let n0_afc = afc_for_state(base_flow, base_np, flowgates[1].normal_rating_mw);
        let lodf = model
            .dc_model
            .compute_lodf_pairs(&[branch_idx, contingency_idx], &[contingency_idx])
            .expect("LODF pair");
        let lodf_lk = lodf
            .get(branch_idx, contingency_idx)
            .expect("monitored/outage lodf");
        let post_flow = (model.base_flows_pu[branch_idx]
            + lodf_lk * model.base_flows_pu[contingency_idx])
            * net.base_mva;
        let post_np = transfer_ptdf
            .get(&branch_idx)
            .copied()
            .expect("monitored branch PTDF")
            + lodf_lk
                * transfer_ptdf
                    .get(&contingency_idx)
                    .copied()
                    .expect("outage branch PTDF");
        let n1_afc = afc_for_state(
            post_flow,
            post_np,
            flowgates[1].effective_contingency_rating_mw(),
        );
        let expected_binding = if n1_afc <= n0_afc {
            Some(contingency_idx)
        } else {
            None
        };
        assert_eq!(results[1].binding_contingency, expected_binding);
    }

    #[test]
    fn test_afc_n1_flowgate_respects_base_case_limit() {
        let net = load_case9();
        let mut model = PreparedTransferModel::new(&net).expect("case9 transfer model");
        let request = AfcRequest {
            path: transfer_path("bus1_to_bus9", vec![1], vec![9]),
            flowgates: vec![Flowgate::new(
                "FG_branch1_base_binds",
                1,
                Some(2),
                model.base_flows_pu[1].abs() * net.base_mva + 1.0,
                Some(10_000.0),
            )],
        };
        let transfer_ptdf = model
            .interface_net_ptdf(&[1, 2], &request.path)
            .expect("transfer PTDF for monitored and contingency branches");
        let base_flow = model.base_flows_pu[1] * net.base_mva;
        let base_np = transfer_ptdf
            .get(&1)
            .copied()
            .expect("monitored branch PTDF");
        let expected_n0_afc = if base_np.abs() < 1e-12 {
            f64::INFINITY
        } else {
            let headroom = if base_np > 0.0 {
                request.flowgates[0].normal_rating_mw - base_flow
            } else {
                request.flowgates[0].normal_rating_mw + base_flow
            };
            (headroom / base_np.abs()).max(0.0)
        };

        let results = model.compute_afc(&request).expect("AFC should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].binding_contingency, None,
            "N-0 headroom should remain active when it is tighter than the N-1 limit"
        );
        assert!(
            (results[0].afc_mw - expected_n0_afc).abs() < 1e-9,
            "base-case headroom should determine AFC: expected {expected_n0_afc}, got {}",
            results[0].afc_mw
        );
    }

    #[test]
    fn test_afc_rejects_out_of_service_monitored_branch() {
        let mut net = load_case9();
        net.branches[8].in_service = false;

        let err = compute_afc(
            &net,
            &AfcRequest {
                path: transfer_path("bus1_to_bus9", vec![1], vec![9]),
                flowgates: vec![Flowgate::new("FG_branch8_offline", 8, None, 250.0, None)],
            },
        )
        .expect_err("out-of-service monitored branch must be rejected");

        assert!(
            err.to_string()
                .contains("monitored branch 8 is out of service"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_afc_rejects_out_of_service_contingency_branch() {
        let mut net = load_case9();
        net.branches[2].in_service = false;

        let err = compute_afc(
            &net,
            &AfcRequest {
                path: transfer_path("bus1_to_bus9", vec![1], vec![9]),
                flowgates: vec![Flowgate::new("FG_bad_ctg", 1, Some(2), 250.0, None)],
            },
        )
        .expect_err("out-of-service contingency branch must be rejected");

        assert!(
            err.to_string()
                .contains("contingency branch 2 is out of service"),
            "unexpected error: {err}"
        );
    }

    // ── Injection capability: large-LODF validation (P1-009) ───────────────

    fn load_case14() -> Network {
        surge_io::load(case_path("case14")).expect("failed to parse case14")
    }

    fn assign_default_branch_ratings(network: &mut Network, rating_mva: f64) {
        for branch in &mut network.branches {
            if branch.rating_a_mva <= 1e-6 {
                branch.rating_a_mva = rating_mva;
            }
        }
    }

    /// P1-009: Validate injection capability for a network where some LODFs
    /// are large, and verify results are within expected bounds.
    #[test]
    fn test_injection_capability_large_lodf_case14() {
        let mut net = load_case14();
        assign_default_branch_ratings(&mut net, 500.0);
        let n_br = net.n_branches();
        let all_branches: Vec<usize> = (0..n_br).collect();
        let lodf =
            compute_lodf_matrix(&net, &LodfMatrixRequest::for_branches(&all_branches)).unwrap();

        let mut count_large = 0usize;
        let mut count_very_large = 0usize;
        for k in 0..n_br {
            for l in 0..n_br {
                if l == k {
                    continue;
                }
                let v = lodf[(l, k)];
                if v.is_finite() && v.abs() > 0.5 {
                    count_large += 1;
                }
                if v.is_finite() && v.abs() > 0.8 {
                    count_very_large += 1;
                }
            }
        }
        assert!(
            count_large > 0,
            "Case14 should have LODF entries > 0.5 to exercise the approximation boundary"
        );

        let cap = inj_cap(&net, 1.0).unwrap();

        for &(bus_num, cap_mw) in &cap.by_bus {
            assert!(
                cap_mw >= 0.0,
                "Injection capability at bus {bus_num} is negative ({cap_mw})"
            );
        }

        let has_finite = cap.by_bus.iter().any(|&(_, v)| v.is_finite() && v > 0.0);
        assert!(
            has_finite,
            "At least one bus should have a finite positive capability"
        );

        let worst = cap
            .by_bus
            .iter()
            .filter(|(_, v)| v.is_finite() && *v > 1e-6)
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

        if let Some(&(worst_bus, worst_cap)) = worst {
            assert!(
                worst_cap > 0.0 && worst_cap <= 500.0 + 1e-9,
                "Worst-bus injection capability at bus {worst_bus} = {worst_cap} \
                 should be positive and bounded for case14"
            );
            eprintln!(
                "P1-009 diagnostic: case14 has {} LODF entries > 0.5, {} > 0.8",
                count_large, count_very_large
            );
            eprintln!(
                "P1-009 diagnostic: worst bus = {worst_bus}, injection cap = {worst_cap:.6} MW"
            );
        }
    }

    /// P1-009: Verify injection capability is monotonically non-decreasing
    /// as the post-contingency rating fraction increases.
    #[test]
    fn test_injection_capability_monotone_in_rating_case14() {
        let mut net = load_case14();
        assign_default_branch_ratings(&mut net, 500.0);
        let fractions = [1.0, 1.1, 1.25, 1.5];
        let mut prev_caps: Option<Vec<(u32, f64)>> = None;

        for &frac in &fractions {
            let cap = inj_cap(&net, frac).unwrap();

            if let Some(ref prev) = prev_caps {
                for ((bus_p, cap_p), (bus_c, cap_c)) in prev.iter().zip(cap.by_bus.iter()) {
                    assert_eq!(bus_p, bus_c, "Bus ordering should be consistent");
                    assert!(
                        *cap_c >= *cap_p - 1e-10,
                        "Injection capability at bus {} decreased from {:.6} to {:.6} \
                         when rating fraction increased (monotonicity violation)",
                        bus_c,
                        cap_p,
                        cap_c,
                    );
                }
            }

            prev_caps = Some(cap.by_bus.clone());
        }
    }
}
