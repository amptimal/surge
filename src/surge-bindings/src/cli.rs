// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CLI argument definitions, enums, and conversion helpers.

use std::path::PathBuf;

use anyhow::Result;
use clap::{ArgAction, Parser, ValueEnum};

fn parse_positive_finite_f64(s: &str) -> Result<f64, String> {
    let v: f64 = s.parse().map_err(|e| format!("invalid number: {e}"))?;
    if !v.is_finite() || v <= 0.0 {
        return Err(format!("must be a positive finite number, got {v}"));
    }
    Ok(v)
}

pub(crate) fn cli_value_name(v: &impl ValueEnum) -> String {
    v.to_possible_value()
        .map(|pv| pv.get_name().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum CliMethod {
    #[value(name = "acpf")]
    Acpf,
    #[value(name = "acpf-warm")]
    AcpfWarm,
    Fdpf,
    #[value(name = "dcpf")]
    Dcpf,
    #[value(name = "dc-opf")]
    DcOpf,
    #[value(name = "ac-opf")]
    AcOpf,
    Scopf,
    Hvdc,
    Contingency,
    #[value(name = "n-2")]
    N2,
    Orpd,
    Ots,
    #[value(name = "injection-capability")]
    InjectionCapability,
    #[value(name = "nerc-atc")]
    NercAtc,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum TextOrJson {
    Text,
    Json,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum TextDetail {
    Auto,
    Summary,
    Full,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedTextDetail {
    Summary,
    Full,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum CliScreeningArg {
    Off,
    Lodf,
    Fdpf,
}

impl CliScreeningArg {
    pub(crate) fn into_runtime(self) -> surge_contingency::ScreeningMode {
        match self {
            Self::Off => surge_contingency::ScreeningMode::Off,
            Self::Lodf => surge_contingency::ScreeningMode::Lodf,
            Self::Fdpf => surge_contingency::ScreeningMode::Fdpf,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum DcOpfWarmStart {
    Auto,
    Yes,
    No,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ScopfFormulationArg {
    Dc,
    Ac,
}

impl ScopfFormulationArg {
    pub(crate) fn into_runtime(self) -> surge_opf::ScopfFormulation {
        match self {
            Self::Dc => surge_opf::ScopfFormulation::Dc,
            Self::Ac => surge_opf::ScopfFormulation::Ac,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ScopfModeArg {
    Preventive,
    Corrective,
}

impl ScopfModeArg {
    pub(crate) fn into_runtime(self) -> surge_opf::ScopfMode {
        match self {
            Self::Preventive => surge_opf::ScopfMode::Preventive,
            Self::Corrective => surge_opf::ScopfMode::Corrective,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ContingencyRatingArg {
    #[value(name = "rate-a")]
    RateA,
    #[value(name = "rate-b")]
    RateB,
    #[value(name = "rate-c")]
    RateC,
}

impl ContingencyRatingArg {
    pub(crate) fn into_runtime(self) -> surge_opf::ThermalRating {
        match self {
            Self::RateA => surge_opf::ThermalRating::RateA,
            Self::RateB => surge_opf::ThermalRating::RateB,
            Self::RateC => surge_opf::ThermalRating::RateC,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum SolverBackend {
    #[value(name = "default")]
    Auto,
    Highs,
    Gurobi,
    Cplex,
    Copt,
    Ipopt,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum HvdcMethodArg {
    Auto,
    Sequential,
    #[value(name = "block_coupled")]
    BlockCoupled,
    Hybrid,
}

impl HvdcMethodArg {
    pub(crate) fn into_runtime(self) -> surge_hvdc::HvdcMethod {
        match self {
            Self::Auto => surge_hvdc::HvdcMethod::Auto,
            Self::Sequential => surge_hvdc::HvdcMethod::Sequential,
            Self::BlockCoupled => surge_hvdc::HvdcMethod::BlockCoupled,
            Self::Hybrid => surge_hvdc::HvdcMethod::Hybrid,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum VoltageStressArg {
    Off,
    Proxy,
    #[value(name = "exact_l_index")]
    ExactLIndex,
}

impl VoltageStressArg {
    pub(crate) fn into_runtime(
        self,
        l_index_threshold: f64,
    ) -> surge_contingency::VoltageStressMode {
        match self {
            Self::Off => surge_contingency::VoltageStressMode::Off,
            Self::Proxy => surge_contingency::VoltageStressMode::Proxy,
            Self::ExactLIndex => {
                surge_contingency::VoltageStressMode::ExactLIndex { l_index_threshold }
            }
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum CliQSharing {
    Capability,
    Mbase,
    Equal,
}

impl CliQSharing {
    pub(crate) fn into_runtime(self) -> surge_ac::QSharingMode {
        match self {
            Self::Capability => surge_ac::QSharingMode::Capability,
            Self::Mbase => surge_ac::QSharingMode::Mbase,
            Self::Equal => surge_ac::QSharingMode::Equal,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum CliAngleReference {
    #[value(name = "preserve-initial", alias = "preserve")]
    PreserveInitial,
    Zero,
    #[value(name = "distributed-load", alias = "distributed")]
    DistributedLoad,
    #[value(name = "distributed-generation")]
    DistributedGeneration,
    #[value(name = "distributed-inertia")]
    DistributedInertia,
}

impl CliAngleReference {
    pub(crate) fn into_runtime(self) -> surge_network::AngleReference {
        match self {
            Self::PreserveInitial => surge_network::AngleReference::PreserveInitial,
            Self::Zero => surge_network::AngleReference::Zero,
            Self::DistributedLoad => surge_network::AngleReference::Distributed(
                surge_network::DistributedAngleWeight::LoadWeighted,
            ),
            Self::DistributedGeneration => surge_network::AngleReference::Distributed(
                surge_network::DistributedAngleWeight::GenerationWeighted,
            ),
            Self::DistributedInertia => surge_network::AngleReference::Distributed(
                surge_network::DistributedAngleWeight::InertiaWeighted,
            ),
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum CliDiscreteMode {
    Continuous,
    #[value(name = "round-and-check")]
    RoundAndCheck,
}

impl CliDiscreteMode {
    pub(crate) fn into_runtime(self) -> surge_opf::DiscreteMode {
        match self {
            Self::Continuous => surge_opf::DiscreteMode::Continuous,
            Self::RoundAndCheck => surge_opf::DiscreteMode::RoundAndCheck,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum CliDcCostMode {
    Qp,
    Lp,
}

#[derive(Parser, Debug)]
#[command(
    name = "surge-solve",
    about = "AC/DC power flow solver",
    version = env!("CARGO_PKG_VERSION")
)]
pub(crate) struct Cli {
    /// Path to the case file (.m, .raw, .cdf, .surge.json, .surge.json.zst, or .surge.bin).
    pub case_file: PathBuf,

    /// Canonical solution method.
    #[arg(short, long, value_enum, default_value_t = CliMethod::Acpf)]
    pub method: CliMethod,

    /// Convergence tolerance in per-unit
    #[arg(short, long, default_value = "1e-8")]
    pub tolerance: f64,

    /// Maximum iterations
    #[arg(long, default_value = "500")]
    pub max_iter: u32,

    /// Use flat start (Vm=1.0, Va=0.0) instead of case data voltages
    #[arg(long)]
    pub flat_start: bool,

    /// Initialize each N-1 contingency from flat start instead of base-case warm start
    #[arg(long)]
    pub cont_flat_start: bool,

    /// Disable reactive power limit enforcement (PV→PQ switching)
    #[arg(long)]
    pub no_q_limits: bool,

    /// Reactive power sharing mode among generators at the same bus.
    #[arg(long, value_enum, default_value_t = CliQSharing::Capability)]
    pub q_sharing: CliQSharing,

    /// Voltage angle reference convention for output angles.
    #[arg(long, value_enum, default_value_t = CliAngleReference::PreserveInitial)]
    pub angle_reference: CliAngleReference,

    /// Output format: text, json
    #[arg(short, long, value_enum, default_value_t = TextOrJson::Text)]
    pub output: TextOrJson,

    /// Text output detail level: auto uses full tables for small cases and summaries for larger ones.
    #[arg(long, value_enum, default_value_t = TextDetail::Auto)]
    pub detail: TextDetail,

    /// Contingency screening mode: off, lodf, fdpf
    #[arg(long, value_enum, default_value_t = CliScreeningArg::Off)]
    pub screening: CliScreeningArg,

    /// Thermal overload threshold (percent of rate_a)
    #[arg(long, default_value = "100.0")]
    pub thermal_threshold: f64,

    /// Maximum NLP iterations for AC-OPF. 0 = auto: max(500, n_buses / 20).
    #[arg(long, default_value = "0")]
    pub ac_opf_max_iter: u32,

    /// Disable generator P-Q capability curve (D-curve) constraints in AC-OPF.
    #[arg(long)]
    pub no_capability_curves: bool,

    /// AC-OPF discrete control mode.
    #[arg(long, value_enum, default_value_t = CliDiscreteMode::Continuous)]
    pub ac_discrete_mode: CliDiscreteMode,

    /// Co-optimize SVC/STATCOM susceptance as continuous NLP variables in AC-OPF.
    #[arg(long)]
    pub optimize_svc: bool,

    /// Co-optimize TCSC compensating reactance as continuous NLP variables in AC-OPF.
    #[arg(long)]
    pub optimize_tcsc: bool,

    /// Seed AC-OPF initial angles from a DC-OPF solution.
    #[arg(long, value_enum, default_value_t = DcOpfWarmStart::Auto)]
    pub dc_opf_warm_start: DcOpfWarmStart,

    /// DC-OPF cost formulation: qp (exact quadratic) or lp (PWL tangent-line approximation).
    #[arg(long, value_enum, default_value_t = CliDcCostMode::Qp)]
    pub dc_cost_mode: CliDcCostMode,

    /// Number of PWL breakpoints per generator when --dc-cost-mode lp is used (default: 20)
    #[arg(long, default_value = "20")]
    pub dc_pwl_breakpoints: usize,

    /// SCOPF formulation: dc (default) or ac
    #[arg(long, value_enum, default_value_t = ScopfFormulationArg::Dc)]
    pub scopf_formulation: ScopfFormulationArg,

    /// SCOPF mode: preventive (default) or corrective
    #[arg(long, value_enum, default_value_t = ScopfModeArg::Preventive)]
    pub scopf_mode: ScopfModeArg,

    /// SCOPF: violation tolerance in per-unit (default: 0.01 = 1 MW at 100 MVA)
    #[arg(long, default_value = "0.01")]
    pub scopf_viol_tol: f64,

    /// SCOPF: maximum cuts to add per iteration
    #[arg(long, default_value = "100")]
    pub scopf_max_cuts: usize,

    /// SCOPF: maximum constraint generation iterations
    #[arg(long, default_value = "20")]
    pub scopf_max_iter: u32,

    /// SCOPF: post-contingency thermal rating (rate-a, rate-b, rate-c)
    #[arg(long, value_enum, default_value_t = ContingencyRatingArg::RateA)]
    pub contingency_rating: ContingencyRatingArg,

    /// SCOPF: disable flowgate and interface constraints.
    #[arg(long = "no-flowgates", action = ArgAction::SetTrue)]
    pub no_flowgates: bool,

    /// SCOPF: disable post-contingency voltage limits in AC-SCOPF.
    #[arg(long = "no-voltage-security", action = ArgAction::SetTrue)]
    pub no_voltage_security: bool,

    /// Export a full solved-state artifact to this file path (JSON or JSON.zst)
    #[arg(long)]
    pub export: Option<PathBuf>,

    /// Convert format override for --convert: matpower, psse33, psse35, surge-json, surge-bin, xiidm, dss, epc, ucte, cgmes, cgmes3
    #[arg(long)]
    pub export_format: Option<String>,

    /// Solver backend.
    #[arg(long, value_enum, default_value_t = SolverBackend::Auto)]
    pub solver: SolverBackend,

    /// ORPD objective: loss, voltage, or combined.
    #[arg(long, default_value = "loss", value_parser = ["loss", "voltage", "combined"])]
    pub orpd_objective: String,

    /// ORPD voltage target in per-unit for voltage and combined objectives.
    #[arg(long, default_value = "1.0")]
    pub orpd_v_ref: f64,

    /// ORPD weight on active losses for the combined objective.
    #[arg(long, default_value = "1.0")]
    pub orpd_loss_weight: f64,

    /// ORPD weight on voltage deviation for the combined objective.
    #[arg(long, default_value = "1.0")]
    pub orpd_voltage_weight: f64,

    /// HVDC solver method: auto, sequential, block_coupled, hybrid (default: auto)
    #[arg(long, value_enum, default_value_t = HvdcMethodArg::Auto)]
    pub hvdc_method: HvdcMethodArg,

    /// Inner DC solver tolerance for explicit DC-network HVDC methods.
    #[arg(long, default_value = "1e-8")]
    pub hvdc_dc_tol: f64,

    /// Inner DC solver iteration limit for explicit DC-network HVDC methods.
    #[arg(long, default_value = "50")]
    pub hvdc_dc_max_iter: u32,

    /// Enable HVDC in AC-OPF (auto-detects from network data if not specified)
    #[arg(long)]
    pub include_hvdc: bool,

    /// Parse the case file and print a summary without solving
    #[arg(long)]
    pub parse_only: bool,

    /// Convert case file to another format without solving (output path)
    #[arg(long)]
    pub convert: Option<PathBuf>,

    /// Contingency voltage stability mode: off, proxy, or exact_l_index
    #[arg(long, value_enum, default_value_t = VoltageStressArg::Proxy)]
    pub voltage_stress_mode: VoltageStressArg,

    /// L-index threshold for exact_l_index voltage stress mode (default: 0.7)
    #[arg(long, default_value = "0.7")]
    pub l_index_threshold: f64,

    /// Enforce area interchange targets from network area schedules.
    #[arg(long)]
    pub enforce_interchange: bool,

    /// Enable discrete controls (PAR, OLTC, switched shunts) in contingency analysis.
    #[arg(long)]
    pub discrete_controls: bool,

    /// Store post-contingency bus voltages and branch flows in JSON output
    #[arg(long)]
    pub store_voltages: bool,

    /// Source bus numbers for transfer path (nerc-atc method)
    #[arg(long, value_delimiter = ',')]
    pub source_buses: Option<Vec<u32>>,

    /// Sink bus numbers for transfer path (nerc-atc method)
    #[arg(long, value_delimiter = ',')]
    pub sink_buses: Option<Vec<u32>>,

    /// Post-contingency rating fraction for injection-capability (default: 1.0)
    #[arg(long, default_value = "1.0", value_parser = parse_positive_finite_f64)]
    pub post_ctg_rating_frac: f64,

    /// Increase logging verbosity (-v = info, -vv = debug, -vvv = trace)
    #[arg(short, long, action = ArgAction::Count)]
    pub verbose: u8,

    /// Quiet mode — suppress all output except errors
    #[arg(short, long)]
    pub quiet: bool,

    /// Log output format: text (default) or json
    #[arg(long, value_enum, default_value_t = TextOrJson::Text)]
    pub log_format: TextOrJson,
}

pub(crate) fn make_lp_solver(
    solver_name: SolverBackend,
) -> Result<Option<std::sync::Arc<dyn surge_opf::backends::LpSolver>>> {
    match solver_name {
        SolverBackend::Auto => Ok(None),
        other => surge_opf::backends::lp_solver_from_str(&cli_value_name(&other))
            .map(Some)
            .map_err(|e| anyhow::anyhow!(e)),
    }
}

/// Select NLP solver override. `default`/empty leaves runtime solver policy intact.
pub(crate) fn make_nlp_solver(
    solver_name: SolverBackend,
) -> Result<Option<std::sync::Arc<dyn surge_opf::backends::NlpSolver>>> {
    match solver_name {
        SolverBackend::Auto => Ok(None),
        other => surge_opf::backends::nlp_solver_from_str(&cli_value_name(&other))
            .map(Some)
            .map_err(|e| anyhow::anyhow!(e)),
    }
}

/// Select AC-OPF NLP solver override, including native AC-OPF-only backends.
pub(crate) fn make_ac_opf_nlp_solver(
    solver_name: SolverBackend,
) -> Result<Option<std::sync::Arc<dyn surge_opf::backends::NlpSolver>>> {
    match solver_name {
        SolverBackend::Auto => Ok(None),
        other => surge_opf::backends::ac_opf_nlp_solver_from_str(&cli_value_name(&other))
            .map(Some)
            .map_err(|e| anyhow::anyhow!(e)),
    }
}

pub(crate) fn parse_orpd_objective(cli: &Cli) -> Result<surge_opf::switching::OrpdObjective> {
    surge_opf::switching::OrpdObjective::parse_named(
        &cli.orpd_objective,
        cli.orpd_v_ref,
        cli.orpd_loss_weight,
        cli.orpd_voltage_weight,
    )
    .map_err(|e| anyhow::anyhow!(e))
}

pub(crate) fn method_uses_nlp_solver(cli: &Cli) -> bool {
    match cli.method {
        CliMethod::AcOpf | CliMethod::Orpd => true,
        CliMethod::Scopf => cli.scopf_formulation == ScopfFormulationArg::Ac,
        _ => false,
    }
}

pub(crate) fn method_uses_lp_solver(cli: &Cli) -> bool {
    matches!(
        cli.method,
        CliMethod::DcOpf | CliMethod::Ots | CliMethod::Scopf
    )
}
