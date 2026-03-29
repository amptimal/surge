// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! DC-SCOPF contingency preparation and initial-cut preloading.

use std::collections::{HashMap, HashSet};

use surge_dc::PtdfRows;
use surge_network::Network;
use surge_network::network::{Contingency, generate_n1_branch_contingencies};
use tracing::{info, warn};

use surge_sparse::Triplet;

use super::dc_support::{
    ContingencyData, CutInfo, CutType, GenContingencyData, N2ContingencyData, get_ptdf,
};
use super::types::{ScopfCutKind, ScopfOptions, ScopfRunContext};

pub(crate) struct PreventiveInitialCuts {
    pub cut_triplets: Vec<Triplet<f64>>,
    pub cut_row_lower: Vec<f64>,
    pub cut_row_upper: Vec<f64>,
    pub cut_metadata: Vec<CutInfo>,
    pub constrained_pairs: HashSet<(usize, usize)>,
    pub cut_slack_cost: Vec<f64>,
    pub cut_slack_lower: Vec<f64>,
    pub cut_slack_upper: Vec<f64>,
    pub pre_screened_count: usize,
    pub pairs_evaluated: usize,
    pub screening_threshold_fraction: f64,
}

pub(crate) struct PreparedPreventiveContingencies {
    pub contingencies: Vec<Contingency>,
    pub monitored_branches: Vec<usize>,
    pub ctg_data: Vec<ContingencyData>,
    pub gen_ctg_data: Vec<GenContingencyData>,
    pub n2_ctg_data: Vec<N2ContingencyData>,
    pub mixed_gen_shifts: HashMap<usize, Vec<usize>>,
    pub initial_cuts: PreventiveInitialCuts,
}

pub(crate) struct PreventiveContingencyInputs<'a> {
    pub bus_map: &'a HashMap<u32, usize>,
    pub gen_indices: &'a [usize],
    pub gen_bus_idx: &'a [usize],
    pub ptdf: &'a PtdfRows,
    pub n_bus: usize,
    pub n_branches: usize,
    pub n_base_rows: usize,
    pub n_var_base: usize,
    pub theta_offset: usize,
    pub thermal_penalty_per_pu: f64,
    pub base: f64,
}

struct BranchCutBuilder<'a> {
    network: &'a Network,
    options: &'a ScopfOptions,
    bus_map: &'a HashMap<u32, usize>,
    base: f64,
    theta_offset: usize,
    n_base_rows: usize,
    n_var_base: usize,
    thermal_penalty_per_pu: f64,
}

impl<'a> BranchCutBuilder<'a> {
    fn push_lodf_cut(
        &self,
        cuts: &mut PreventiveInitialCuts,
        ctg_idx: usize,
        monitored_branch_idx: usize,
        outaged: &ContingencyData,
        lodf_lk: f64,
    ) {
        let cut_idx = cuts.cut_metadata.len();
        let cut_row = self.n_base_rows + cut_idx;
        let s_up_col = self.n_var_base + 2 * cut_idx;
        let s_lo_col = self.n_var_base + 2 * cut_idx + 1;

        let br_m = &self.network.branches[monitored_branch_idx];
        let b_m = br_m.b_dc();
        let from_m = self.bus_map[&br_m.from_bus];
        let to_m = self.bus_map[&br_m.to_bus];

        let br_k = &self.network.branches[outaged.outaged_br];
        let b_k = br_k.b_dc();

        cuts.cut_triplets.push(Triplet {
            row: cut_row,
            col: self.theta_offset + from_m,
            val: b_m,
        });
        cuts.cut_triplets.push(Triplet {
            row: cut_row,
            col: self.theta_offset + to_m,
            val: -b_m,
        });
        cuts.cut_triplets.push(Triplet {
            row: cut_row,
            col: self.theta_offset + outaged.from_bus_idx,
            val: lodf_lk * b_k,
        });
        cuts.cut_triplets.push(Triplet {
            row: cut_row,
            col: self.theta_offset + outaged.to_bus_idx,
            val: -lodf_lk * b_k,
        });
        cuts.cut_triplets.push(Triplet {
            row: cut_row,
            col: s_up_col,
            val: -1.0,
        });
        cuts.cut_triplets.push(Triplet {
            row: cut_row,
            col: s_lo_col,
            val: 1.0,
        });

        let pfinj_m = b_m * br_m.phase_shift_rad;
        let pfinj_k = b_k * br_k.phase_shift_rad;
        let fmax_m = self
            .options
            .contingency_rating
            .of(&self.network.branches[monitored_branch_idx])
            / self.base;
        cuts.cut_row_lower
            .push(-fmax_m - pfinj_m - lodf_lk * pfinj_k);
        cuts.cut_row_upper
            .push(fmax_m - pfinj_m - lodf_lk * pfinj_k);

        cuts.cut_slack_cost
            .extend_from_slice(&[self.thermal_penalty_per_pu, self.thermal_penalty_per_pu]);
        cuts.cut_slack_lower.extend_from_slice(&[0.0, 0.0]);
        cuts.cut_slack_upper
            .extend_from_slice(&[f64::INFINITY, f64::INFINITY]);

        cuts.cut_metadata.push(CutInfo {
            ctg_idx,
            monitored_branch_idx,
            outaged_branch_indices: vec![outaged.outaged_br],
            lodf_lk,
            cut_type: CutType::BranchThermal,
            gen_local_idx: None,
        });
    }
}

pub(crate) fn prepare_preventive_contingencies(
    network: &Network,
    options: &ScopfOptions,
    context: &ScopfRunContext,
    inputs: PreventiveContingencyInputs<'_>,
) -> PreparedPreventiveContingencies {
    let contingencies = options
        .contingencies
        .clone()
        .unwrap_or_else(|| generate_n1_branch_contingencies(network));

    let monitored_branches: Vec<usize> = (0..inputs.n_branches)
        .filter(|&branch_idx| {
            let br = &network.branches[branch_idx];
            br.in_service && br.rating_a_mva >= options.min_rate_a
        })
        .collect();

    let mut ctg_data = Vec::with_capacity(contingencies.len());
    let mut gen_ctg_data = Vec::new();
    let mut n2_ctg_data = Vec::new();
    let mut mixed_gen_shifts: HashMap<usize, Vec<usize>> = HashMap::new();

    for (ctg_idx, ctg) in contingencies.iter().enumerate() {
        let n_branches = ctg.branch_indices.len();
        let n_gens = ctg.generator_indices.len();

        if n_branches > 0 && n_gens > 0 {
            let mut gen_locals = Vec::new();
            for &gen_idx in &ctg.generator_indices {
                if gen_idx < network.generators.len()
                    && network.generators[gen_idx].in_service
                    && let Some(local) = inputs.gen_indices.iter().position(|&g| g == gen_idx)
                {
                    gen_locals.push(local);
                }
            }
            if !gen_locals.is_empty() {
                mixed_gen_shifts.insert(ctg_idx, gen_locals);
            }
        }

        if n_branches == 1 {
            let outaged_br = ctg.branch_indices[0];
            if outaged_br >= inputs.n_branches || !network.branches[outaged_br].in_service {
                continue;
            }

            let br = &network.branches[outaged_br];
            let from_idx = inputs.bus_map[&br.from_bus];
            let to_idx = inputs.bus_map[&br.to_bus];
            let ptdf_diff_k = get_ptdf(inputs.ptdf, outaged_br, from_idx)
                - get_ptdf(inputs.ptdf, outaged_br, to_idx);
            let denom = 1.0 - ptdf_diff_k;
            if denom.abs() < 1e-10 {
                continue;
            }

            ctg_data.push(ContingencyData {
                ctg_idx,
                outaged_br,
                from_bus_idx: from_idx,
                to_bus_idx: to_idx,
                denom,
                label: ctg.label.clone(),
            });
            continue;
        }

        if n_branches == 2 {
            let k1 = ctg.branch_indices[0];
            let k2 = ctg.branch_indices[1];
            if k1 >= inputs.n_branches
                || k2 >= inputs.n_branches
                || !network.branches[k1].in_service
                || !network.branches[k2].in_service
            {
                continue;
            }

            let br1 = &network.branches[k1];
            let br2 = &network.branches[k2];
            let k1_from = inputs.bus_map[&br1.from_bus];
            let k1_to = inputs.bus_map[&br1.to_bus];
            let k2_from = inputs.bus_map[&br2.from_bus];
            let k2_to = inputs.bus_map[&br2.to_bus];

            let ptdf_diff_k1 =
                get_ptdf(inputs.ptdf, k1, k1_from) - get_ptdf(inputs.ptdf, k1, k1_to);
            let denom_k1 = 1.0 - ptdf_diff_k1;
            let ptdf_diff_k2 =
                get_ptdf(inputs.ptdf, k2, k2_from) - get_ptdf(inputs.ptdf, k2, k2_to);
            let denom_k2 = 1.0 - ptdf_diff_k2;
            if denom_k1.abs() < 1e-10 || denom_k2.abs() < 1e-10 {
                continue;
            }

            let ptdf_diff_k1_k2 =
                get_ptdf(inputs.ptdf, k1, k2_from) - get_ptdf(inputs.ptdf, k1, k2_to);
            let lodf_k1k2 = ptdf_diff_k1_k2 / denom_k2;
            let ptdf_diff_k2_k1 =
                get_ptdf(inputs.ptdf, k2, k1_from) - get_ptdf(inputs.ptdf, k2, k1_to);
            let lodf_k2k1 = ptdf_diff_k2_k1 / denom_k1;

            let compound_denom = 1.0 - lodf_k1k2 * lodf_k2k1;
            if compound_denom.abs() < 1e-10 {
                if n_gens == 0 {
                    warn!(
                        k1,
                        k2,
                        compound_denom,
                        "N-2 compound denom invalid — disconnecting cut-set, skipping"
                    );
                }
                continue;
            }

            n2_ctg_data.push(N2ContingencyData {
                ctg_idx,
                k1,
                k2,
                k1_from,
                k1_to,
                k2_from,
                k2_to,
                denom_k1,
                denom_k2,
                lodf_k1k2,
                lodf_k2k1,
                compound_denom,
            });
            continue;
        }

        if n_branches == 0 && n_gens >= 1 {
            for &gen_idx in &ctg.generator_indices {
                if gen_idx >= network.generators.len() || !network.generators[gen_idx].in_service {
                    continue;
                }
                let generator = &network.generators[gen_idx];
                if let Some(&bus_idx) = inputs.bus_map.get(&generator.bus)
                    && let Some(local) = inputs.gen_indices.iter().position(|&g| g == gen_idx)
                {
                    gen_ctg_data.push(GenContingencyData {
                        ctg_idx,
                        gen_local: local,
                        bus_idx,
                        label: ctg.label.clone(),
                    });
                }
            }
            continue;
        }

        if n_branches > 2 {
            warn!(
                ctg_id = %ctg.id,
                branches = n_branches,
                "N-{n_branches} branch contingency not supported in DC-SCOPF — skipping"
            );
        }
    }

    if !gen_ctg_data.is_empty() {
        info!(
            gen_contingencies = gen_ctg_data.len(),
            "SCOPF: generator contingencies prepared for PTDF-based screening"
        );
    }
    if !n2_ctg_data.is_empty() {
        info!(
            n2_contingencies = n2_ctg_data.len(),
            "SCOPF: N-2 multi-branch contingencies prepared for Woodbury LODF"
        );
    }
    if !mixed_gen_shifts.is_empty() {
        info!(
            mixed = mixed_gen_shifts.len(),
            "SCOPF: mixed branch+gen contingencies with gen-trip PTDF shifts"
        );
    }

    let mut initial_cuts = PreventiveInitialCuts {
        cut_triplets: Vec::new(),
        cut_row_lower: Vec::new(),
        cut_row_upper: Vec::new(),
        cut_metadata: Vec::new(),
        constrained_pairs: HashSet::new(),
        cut_slack_cost: Vec::new(),
        cut_slack_lower: Vec::new(),
        cut_slack_upper: Vec::new(),
        pre_screened_count: 0,
        pairs_evaluated: 0,
        screening_threshold_fraction: options
            .screener
            .as_ref()
            .map_or(1.0, |s| s.threshold_fraction),
    };
    let cut_builder = BranchCutBuilder {
        network,
        options,
        bus_map: inputs.bus_map,
        base: inputs.base,
        theta_offset: inputs.theta_offset,
        n_base_rows: inputs.n_base_rows,
        n_var_base: inputs.n_var_base,
        thermal_penalty_per_pu: inputs.thermal_penalty_per_pu,
    };

    if let Some(ref warm_start) = context.runtime.warm_start {
        let mut warm_start_added = 0usize;
        for cut in &warm_start.active_cuts {
            if ctg_data.is_empty() {
                break;
            }
            if cut.cut_kind != ScopfCutKind::BranchThermal || cut.outaged_branch_indices.len() != 1
            {
                continue;
            }
            let monitored_branch_idx = cut.monitored_branch_idx;
            let Some(contingency) = ctg_data
                .iter()
                .find(|contingency| contingency.outaged_br == cut.outaged_branch_indices[0])
            else {
                continue;
            };
            if monitored_branch_idx == contingency.outaged_br {
                continue;
            }
            if initial_cuts
                .constrained_pairs
                .contains(&(contingency.ctg_idx, monitored_branch_idx))
            {
                continue;
            }

            let ptdf_diff_m = get_ptdf(inputs.ptdf, monitored_branch_idx, contingency.from_bus_idx)
                - get_ptdf(inputs.ptdf, monitored_branch_idx, contingency.to_bus_idx);
            let lodf_lk = ptdf_diff_m / contingency.denom;
            if !lodf_lk.is_finite() {
                continue;
            }

            initial_cuts
                .constrained_pairs
                .insert((contingency.ctg_idx, monitored_branch_idx));
            cut_builder.push_lodf_cut(
                &mut initial_cuts,
                contingency.ctg_idx,
                monitored_branch_idx,
                contingency,
                lodf_lk,
            );
            warm_start_added += 1;
        }

        if warm_start_added > 0 {
            info!(
                "SCOPF warm-start: pre-loaded {} active cuts from prior solve",
                warm_start_added
            );
        }
    }

    if let Some(ref screener) = options.screener
        && options.dc_opf.enforce_thermal_limits
        && !monitored_branches.is_empty()
    {
        let bus_pd_mw = network.bus_load_p_mw();
        let mut net_inject: Vec<f64> = bus_pd_mw.iter().map(|&pd| -pd / inputs.base).collect();
        for (local_gen_idx, &gen_idx) in inputs.gen_indices.iter().enumerate() {
            let generator = &network.generators[gen_idx];
            let pg_est = (generator.pmin + generator.pmax) * 0.5 / inputs.base;
            net_inject[inputs.gen_bus_idx[local_gen_idx]] += pg_est;
        }

        let mut approx_flow = vec![0.0f64; inputs.n_branches];
        for (branch_idx, flow) in approx_flow.iter_mut().enumerate() {
            if !network.branches[branch_idx].in_service {
                continue;
            }
            for (bus_idx, injection) in net_inject.iter().enumerate().take(inputs.n_bus) {
                *flow += get_ptdf(inputs.ptdf, branch_idx, bus_idx) * injection;
            }
        }

        struct ScreenedPair {
            ctg_idx: usize,
            monitored_branch_idx: usize,
            severity: f64,
            lodf_lk: f64,
        }

        let mut candidates = Vec::new();
        for contingency in &ctg_data {
            let flow_k = approx_flow[contingency.outaged_br];
            for &monitored_branch_idx in &monitored_branches {
                if monitored_branch_idx == contingency.outaged_br {
                    continue;
                }

                initial_cuts.pairs_evaluated += 1;

                let ptdf_diff_m =
                    get_ptdf(inputs.ptdf, monitored_branch_idx, contingency.from_bus_idx)
                        - get_ptdf(inputs.ptdf, monitored_branch_idx, contingency.to_bus_idx);
                let lodf_mk = ptdf_diff_m / contingency.denom;
                if !lodf_mk.is_finite() {
                    continue;
                }

                let post_flow = approx_flow[monitored_branch_idx] + lodf_mk * flow_k;
                let f_max = options
                    .contingency_rating
                    .of(&network.branches[monitored_branch_idx])
                    / inputs.base;
                if f_max <= 0.0 {
                    continue;
                }

                let loading = post_flow.abs() / f_max;
                if loading > screener.threshold_fraction {
                    candidates.push(ScreenedPair {
                        ctg_idx: contingency.ctg_idx,
                        monitored_branch_idx,
                        severity: loading - screener.threshold_fraction,
                        lodf_lk: lodf_mk,
                    });
                }
            }
        }

        candidates.sort_by(|a, b| {
            b.severity
                .partial_cmp(&a.severity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for screened in candidates
            .iter()
            .take(candidates.len().min(screener.max_initial_contingencies))
        {
            if initial_cuts
                .constrained_pairs
                .contains(&(screened.ctg_idx, screened.monitored_branch_idx))
            {
                continue;
            }
            let contingency = ctg_data
                .iter()
                .find(|cd| cd.ctg_idx == screened.ctg_idx)
                .expect("ctg_idx from screener must exist in ctg_data");
            initial_cuts
                .constrained_pairs
                .insert((screened.ctg_idx, screened.monitored_branch_idx));
            cut_builder.push_lodf_cut(
                &mut initial_cuts,
                screened.ctg_idx,
                screened.monitored_branch_idx,
                contingency,
                screened.lodf_lk,
            );
        }

        initial_cuts.pre_screened_count = initial_cuts.cut_metadata.len();
        info!(
            "SCOPF pre-screening: {} pairs evaluated, {} constraints pre-loaded (threshold={:.0}%)",
            initial_cuts.pairs_evaluated,
            initial_cuts.pre_screened_count,
            screener.threshold_fraction * 100.0
        );
    }

    PreparedPreventiveContingencies {
        contingencies,
        monitored_branches,
        ctg_data,
        gen_ctg_data,
        n2_ctg_data,
        mixed_gen_shifts,
        initial_cuts,
    }
}
