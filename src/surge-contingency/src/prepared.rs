// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Configured contingency studies and prepared corrective redispatch kernels.

use std::collections::HashMap;

use crate::scrd::{ScrdOptions, ScrdStatus, ScrdViolation, solve_scrd};
use surge_dc::{DcAnalysisRequest, PreparedDcStudy, PtdfRows};
use surge_network::Network;
use surge_network::market::PenaltyConfig;

use crate::{
    ContingencyAnalysis, ContingencyError, ContingencyOptions, Violation, analyze_n1_branch,
    analyze_n1_generator, analyze_n2_branch,
};

/// Study family for a prepared contingency analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContingencyStudyKind {
    /// Single branch outage study.
    N1Branch,
    /// Single generator outage study.
    N1Generator,
    /// Double branch outage study.
    N2Branch,
}

impl ContingencyStudyKind {
    /// Stable string label for API layers.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::N1Branch => "n1_branch",
            Self::N1Generator => "n1_generator",
            Self::N2Branch => "n2_branch",
        }
    }
}

/// Summary of a corrective-redispatch run for one contingency.
#[derive(Debug, Clone)]
pub struct CorrectiveDispatchResult {
    /// Contingency identifier.
    pub contingency_id: String,
    /// Solve status for the redispatch LP.
    pub status: ScrdStatus,
    /// Sum of absolute redispatch across generators (MW).
    pub total_redispatch_mw: f64,
    /// Total redispatch cost.
    pub total_cost: f64,
    /// Number of modeled violations that were resolved.
    pub violations_resolved: usize,
    /// Number of violations that remained unresolved.
    pub unresolvable_violations: usize,
}

/// Prepared DC sensitivity state for repeated corrective-dispatch runs on one network.
pub struct PreparedCorrectiveDispatchStudy<'a> {
    network: &'a Network,
    dc_model: PreparedDcStudy<'a>,
    base_flows_mw: Vec<f64>,
    ptdf_rows: PtdfRows,
}

impl<'a> PreparedCorrectiveDispatchStudy<'a> {
    /// Prepare reusable corrective-dispatch sensitivity state.
    pub fn new(network: &'a Network) -> Result<Self, ContingencyError> {
        let mut dc_model = PreparedDcStudy::new(network)
            .map_err(|e| ContingencyError::DcSolveFailed(e.to_string()))?;
        let all_branches: Vec<usize> = (0..network.n_branches()).collect();
        let bus_map = network.bus_index_map();
        let mut ptdf_bus_indices: Vec<usize> = network
            .generators
            .iter()
            .filter(|generator| generator.in_service)
            .filter_map(|generator| bus_map.get(&generator.bus).copied())
            .collect();
        ptdf_bus_indices.sort_unstable();
        ptdf_bus_indices.dedup();

        let dc_workflow = dc_model
            .run_analysis(
                &DcAnalysisRequest::with_monitored_branches(&all_branches)
                    .with_ptdf_buses(&ptdf_bus_indices),
            )
            .map_err(|e| ContingencyError::DcSolveFailed(e.to_string()))?;

        Ok(Self {
            network,
            base_flows_mw: dc_workflow
                .power_flow
                .branch_p_flow
                .iter()
                .map(|&flow| flow * network.base_mva)
                .collect(),
            ptdf_rows: dc_workflow.ptdf,
            dc_model,
        })
    }

    /// Solve corrective redispatch for the supplied contingency analysis.
    pub fn solve(
        &mut self,
        contingency_analysis: &ContingencyAnalysis,
        penalty_config: Option<PenaltyConfig>,
    ) -> Result<Vec<CorrectiveDispatchResult>, ContingencyError> {
        let mut lodf_columns = self.dc_model.lodf_columns();
        let mut results = Vec::new();

        for contingency in &contingency_analysis.results {
            let Some((thermal_violations, lodf_pairs)) =
                build_scrd_inputs(contingency, &mut lodf_columns)?
            else {
                continue;
            };

            let scrd_options = ScrdOptions {
                violations: thermal_violations.clone(),
                penalty_config: penalty_config.clone(),
                ..ScrdOptions::default()
            };

            let summary = match solve_scrd(
                self.network,
                &self.base_flows_mw,
                crate::scrd::ScrdSensitivityModel {
                    ptdf_rows: &self.ptdf_rows,
                    lodf_pairs: &lodf_pairs,
                },
                &scrd_options,
            ) {
                Ok(solution) => CorrectiveDispatchResult {
                    contingency_id: contingency.id.clone(),
                    status: solution.status,
                    total_redispatch_mw: solution.total_redispatch_mw,
                    total_cost: solution.total_cost,
                    violations_resolved: solution.violations_resolved,
                    unresolvable_violations: solution.unresolvable_violations,
                },
                Err(_error) => CorrectiveDispatchResult {
                    contingency_id: contingency.id.clone(),
                    status: ScrdStatus::SolverError,
                    total_redispatch_mw: 0.0,
                    total_cost: 0.0,
                    violations_resolved: 0,
                    unresolvable_violations: thermal_violations.len(),
                },
            };
            results.push(summary);
        }

        Ok(results)
    }
}

#[allow(clippy::type_complexity)]
fn build_scrd_inputs<'model, 'network>(
    contingency: &crate::ContingencyResult,
    lodf_columns: &mut surge_dc::streaming::LodfColumnBuilder<'model, 'network>,
) -> Result<Option<(Vec<ScrdViolation>, HashMap<(usize, usize), f64>)>, ContingencyError> {
    let contingency_branch = if contingency.branch_indices.len() == 1 {
        contingency.branch_indices.first().copied()
    } else {
        None
    };

    let thermal_violations: Vec<ScrdViolation> = contingency
        .violations
        .iter()
        .filter_map(|violation| {
            if let Violation::ThermalOverload {
                branch_idx,
                flow_mw,
                limit_mva,
                ..
            } = violation
            {
                Some(ScrdViolation {
                    branch_index: *branch_idx,
                    contingency_branch,
                    flow_mw: *flow_mw,
                    rating_mw: *limit_mva,
                })
            } else {
                None
            }
        })
        .collect();

    if thermal_violations.is_empty() {
        return Ok(None);
    }

    let mut lodf_pairs: HashMap<(usize, usize), f64> = HashMap::new();
    if let Some(outage_branch) = contingency_branch {
        let mut monitored_branches: Vec<usize> = thermal_violations
            .iter()
            .map(|violation| violation.branch_index)
            .collect();
        monitored_branches.sort_unstable();
        monitored_branches.dedup();

        let column = lodf_columns
            .compute_column(&monitored_branches, outage_branch)
            .map_err(|e| ContingencyError::DcSolveFailed(e.to_string()))?;
        for (position, &monitored_branch) in monitored_branches.iter().enumerate() {
            lodf_pairs.insert((monitored_branch, outage_branch), column[position]);
        }
    }

    Ok(Some((thermal_violations, lodf_pairs)))
}

/// Prepare reusable corrective-dispatch sensitivity state for one network.
pub fn prepare_corrective_dispatch_study(
    network: &Network,
) -> Result<PreparedCorrectiveDispatchStudy<'_>, ContingencyError> {
    PreparedCorrectiveDispatchStudy::new(network)
}

/// Configured contingency study with optional cached analysis and corrective redispatch state.
pub struct ContingencyStudy<'a> {
    kind: ContingencyStudyKind,
    network: &'a Network,
    options: ContingencyOptions,
    last_analysis: Option<ContingencyAnalysis>,
    corrective_dispatch_study: Option<PreparedCorrectiveDispatchStudy<'a>>,
}

impl<'a> ContingencyStudy<'a> {
    fn build(
        network: &'a Network,
        options: &ContingencyOptions,
        kind: ContingencyStudyKind,
    ) -> Result<Self, ContingencyError> {
        if options.corrective_dispatch {
            return Err(ContingencyError::InvalidOptions(
                "contingency study objects do not embed corrective redispatch; use solve_corrective_dispatch() explicitly after analyze()".to_string(),
            ));
        }
        Ok(Self {
            kind,
            network,
            options: options.clone(),
            last_analysis: None,
            corrective_dispatch_study: None,
        })
    }

    /// Prepare an N-1 branch contingency study.
    pub fn n1_branch(
        network: &'a Network,
        options: &ContingencyOptions,
    ) -> Result<Self, ContingencyError> {
        Self::build(network, options, ContingencyStudyKind::N1Branch)
    }

    /// Prepare an N-1 generator contingency study.
    pub fn n1_generator(
        network: &'a Network,
        options: &ContingencyOptions,
    ) -> Result<Self, ContingencyError> {
        Self::build(network, options, ContingencyStudyKind::N1Generator)
    }

    /// Prepare an N-2 branch contingency study.
    pub fn n2_branch(
        network: &'a Network,
        options: &ContingencyOptions,
    ) -> Result<Self, ContingencyError> {
        Self::build(network, options, ContingencyStudyKind::N2Branch)
    }

    /// Study family for this prepared analysis.
    pub fn kind(&self) -> ContingencyStudyKind {
        self.kind
    }

    /// Run the configured study and cache the latest analysis.
    pub fn analyze(&mut self) -> Result<&ContingencyAnalysis, ContingencyError> {
        let analysis = match self.kind {
            ContingencyStudyKind::N1Branch => analyze_n1_branch(self.network, &self.options)?,
            ContingencyStudyKind::N1Generator => analyze_n1_generator(self.network, &self.options)?,
            ContingencyStudyKind::N2Branch => analyze_n2_branch(self.network, &self.options)?,
        };
        self.last_analysis = Some(analysis);
        Ok(self
            .last_analysis
            .as_ref()
            .expect("analysis is cached immediately after execution"))
    }

    /// Borrow the latest cached analysis, if the study has been run.
    pub fn analysis(&self) -> Option<&ContingencyAnalysis> {
        self.last_analysis.as_ref()
    }

    /// Run the configured study and return an owned analysis snapshot.
    pub fn analyze_cloned(&mut self) -> Result<ContingencyAnalysis, ContingencyError> {
        self.analyze().cloned()
    }

    /// Borrow the options used to configure the study.
    pub fn options(&self) -> &ContingencyOptions {
        &self.options
    }

    /// Solve corrective redispatch from the latest cached contingency analysis.
    pub fn solve_corrective_dispatch(
        &mut self,
    ) -> Result<Vec<CorrectiveDispatchResult>, ContingencyError> {
        if self.corrective_dispatch_study.is_none() {
            self.corrective_dispatch_study =
                Some(PreparedCorrectiveDispatchStudy::new(self.network)?);
        }
        if self.last_analysis.is_none() {
            let _ = self.analyze()?;
        }
        self.corrective_dispatch_study
            .as_mut()
            .expect("corrective dispatch study initialized")
            .solve(
                self.last_analysis
                    .as_ref()
                    .expect("analysis is available before corrective dispatch"),
                self.options.penalty_config.clone(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{data_available, load_case};
    use surge_ac::solve_ac_pf_kernel;
    use surge_solution::SolveStatus;

    fn fake_contingency_result(
        branch_indices: Vec<usize>,
        generator_indices: Vec<usize>,
        monitored_branch: usize,
        flow_mw: f64,
    ) -> crate::ContingencyResult {
        crate::ContingencyResult {
            id: "synthetic".into(),
            label: "synthetic".into(),
            branch_indices,
            generator_indices,
            status: crate::ContingencyStatus::Converged,
            converged: true,
            iterations: 5,
            violations: vec![Violation::ThermalOverload {
                branch_idx: monitored_branch,
                from_bus: 1,
                to_bus: 2,
                loading_pct: 120.0,
                flow_mw,
                flow_mva: flow_mw.abs(),
                limit_mva: 100.0,
            }],
            n_islands: 1,
            ..Default::default()
        }
    }

    #[test]
    fn test_prepared_scrd_inputs_generator_outage_uses_measured_flow_path() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let mut prepared = PreparedCorrectiveDispatchStudy::new(&net).expect("prepare SCRD");
        let mut lodf_columns = prepared.dc_model.lodf_columns();
        let result = fake_contingency_result(vec![], vec![0], 1, -125.0);

        let (violations, lodf_pairs) = build_scrd_inputs(&result, &mut lodf_columns)
            .expect("build SCRD inputs")
            .expect("thermal violation present");

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].contingency_branch, None);
        assert_eq!(violations[0].flow_mw, -125.0);
        assert!(
            lodf_pairs.is_empty(),
            "generator outages must not request single-outage LODF pairs"
        );
    }

    #[test]
    fn test_prepared_scrd_inputs_multi_outage_uses_measured_flow_path() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let mut prepared = PreparedCorrectiveDispatchStudy::new(&net).expect("prepare SCRD");
        let mut lodf_columns = prepared.dc_model.lodf_columns();
        let result = fake_contingency_result(vec![0, 1], vec![], 2, 140.0);

        let (violations, lodf_pairs) = build_scrd_inputs(&result, &mut lodf_columns)
            .expect("build SCRD inputs")
            .expect("thermal violation present");

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].contingency_branch, None);
        assert!(
            lodf_pairs.is_empty(),
            "multi-outage SCRD must not use single-outage LODF pairs"
        );
    }

    #[test]
    fn test_prepared_scrd_inputs_single_branch_outage_builds_lodf_pairs() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let mut prepared = PreparedCorrectiveDispatchStudy::new(&net).expect("prepare SCRD");
        let mut lodf_columns = prepared.dc_model.lodf_columns();
        let result = fake_contingency_result(vec![0], vec![], 1, 140.0);

        let (violations, lodf_pairs) = build_scrd_inputs(&result, &mut lodf_columns)
            .expect("build SCRD inputs")
            .expect("thermal violation present");

        assert_eq!(violations[0].contingency_branch, Some(0));
        assert!(
            lodf_pairs.contains_key(&(1, 0)),
            "single-branch outage SCRD should cache LODF(monitored=1, outage=0)"
        );
    }

    #[test]
    fn test_contingency_study_solve_corrective_dispatch_auto_analyzes_generator_study() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let nr = solve_ac_pf_kernel(&net, &surge_ac::AcPfOptions::default()).expect("base solve");
        assert_eq!(nr.status, SolveStatus::Converged);

        let options = ContingencyOptions::default();
        let mut study =
            ContingencyStudy::n1_generator(&net, &options).expect("build generator study");
        let results = study
            .solve_corrective_dispatch()
            .expect("generator study should auto-analyze before SCRD");
        assert!(
            study.analysis().is_some(),
            "solve_corrective_dispatch should populate cached analysis"
        );
        assert!(
            study
                .analysis()
                .expect("analysis cached")
                .summary
                .total_contingencies
                > 0
        );
        assert!(results.len() <= study.analysis().expect("analysis cached").results.len());
    }
}
