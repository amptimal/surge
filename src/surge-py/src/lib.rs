// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Python bindings for Surge — AC/DC power systems solver.
//
// PyO3 binding functions naturally have many parameters (one per Python kwarg)
// and getter names like `from_bus` that conflict with Rust naming conventions.
#![allow(
    clippy::too_many_arguments,
    clippy::wrong_self_convention,
    clippy::type_complexity
)]

mod contingency;
mod dispatch;
mod exceptions;
mod go_c3;
mod hvdc;
mod io;
mod market;
mod matrices;
mod network;
mod opf;
mod pf;
mod prepared_pf;
mod solutions;
mod stability;
mod topology;
mod transfer;
mod utils;

pub mod input_types;
pub mod network_edit;
pub mod parameter_sweep;
pub mod rich_objects;
pub mod test_networks;

pub use network::Network;

use pyo3::prelude::*;

#[pymodule]
fn _surge(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Exceptions
    m.add("SurgeError", m.py().get_type::<exceptions::SurgeError>())?;
    m.add(
        "ConvergenceError",
        m.py().get_type::<exceptions::ConvergenceError>(),
    )?;
    m.add(
        "InfeasibleError",
        m.py().get_type::<exceptions::InfeasibleError>(),
    )?;
    m.add(
        "UnsupportedFeatureError",
        m.py().get_type::<exceptions::UnsupportedFeatureError>(),
    )?;
    m.add(
        "NetworkError",
        m.py().get_type::<exceptions::NetworkError>(),
    )?;
    m.add(
        "TopologyError",
        m.py().get_type::<exceptions::TopologyError>(),
    )?;
    m.add(
        "MissingTopologyError",
        m.py().get_type::<exceptions::MissingTopologyError>(),
    )?;
    m.add(
        "StaleTopologyError",
        m.py().get_type::<exceptions::StaleTopologyError>(),
    )?;
    m.add(
        "AmbiguousTopologyError",
        m.py().get_type::<exceptions::AmbiguousTopologyError>(),
    )?;
    m.add(
        "TopologyIntegrityError",
        m.py().get_type::<exceptions::TopologyIntegrityError>(),
    )?;
    m.add(
        "SurgeIOError",
        m.py().get_type::<exceptions::SurgeIOError>(),
    )?;

    // Utils
    m.add_function(wrap_pyfunction!(utils::init_logging, m)?)?;
    m.add_function(wrap_pyfunction!(utils::set_max_threads, m)?)?;
    m.add_function(wrap_pyfunction!(utils::get_max_threads, m)?)?;

    // Network
    m.add_class::<network::Network>()?;
    m.add_class::<hvdc::HvdcView>()?;
    topology::register(m)?;

    // Solutions / result types
    m.add_class::<solutions::AcPfResult>()?;
    m.add_class::<solutions::DcPfResult>()?;
    m.add_class::<solutions::OpfSolution>()?;
    m.add_class::<dispatch::ActivsgTimeSeries>()?;
    m.add_class::<dispatch::DispatchResult>()?;
    m.add_class::<solutions::BindingContingency>()?;
    m.add_class::<solutions::ContingencyViolation>()?;
    m.add_class::<solutions::FailedContingencyEvaluation>()?;
    m.add_class::<solutions::ScopfScreeningStats>()?;
    m.add_class::<solutions::DcOpfResult>()?;
    m.add_class::<solutions::ScopfResult>()?;
    m.add_class::<solutions::AcOpfHvdcResult>()?;
    m.add_class::<solutions::AcOpfBendersSubproblemResult>()?;
    m.add_class::<solutions::OtsResult>()?;
    m.add_class::<solutions::OrpdResult>()?;
    m.add_class::<solutions::ContingencyAnalysis>()?;
    m.add_class::<solutions::HvdcLccDetail>()?;
    m.add_class::<solutions::HvdcStationSolution>()?;
    m.add_class::<solutions::HvdcDcBusSolution>()?;
    m.add_class::<solutions::HvdcSolution>()?;
    m.add_class::<prepared_pf::PreparedAcPf>()?;

    // I/O
    m.add_function(wrap_pyfunction!(io::version, m)?)?;
    m.add_function(wrap_pyfunction!(io::load, m)?)?;
    m.add_function(wrap_pyfunction!(io::save, m)?)?;
    m.add_function(wrap_pyfunction!(io::load_as, m)?)?;
    m.add_function(wrap_pyfunction!(io::loads, m)?)?;
    m.add_function(wrap_pyfunction!(io::loads_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(io::save_as, m)?)?;
    m.add_function(wrap_pyfunction!(io::dumps, m)?)?;
    m.add_function(wrap_pyfunction!(io::dumps_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(io::io_json_save, m)?)?;
    m.add_function(wrap_pyfunction!(io::io_json_dumps, m)?)?;
    m.add_function(wrap_pyfunction!(io::io_cgmes_save, m)?)?;
    m.add_function(wrap_pyfunction!(io::io_cgmes_to_profiles, m)?)?;
    m.add_function(wrap_pyfunction!(io::io_export_write_network_csv, m)?)?;
    m.add_function(wrap_pyfunction!(io::io_export_write_solution_snapshot, m)?)?;
    m.add_function(wrap_pyfunction!(io::io_geo_apply_bus_coordinates, m)?)?;
    m.add_function(wrap_pyfunction!(io::io_profiles_read_load_csv, m)?)?;
    m.add_function(wrap_pyfunction!(io::io_profiles_read_renewable_csv, m)?)?;
    m.add_class::<io::CgmesProfiles>()?;
    m.add_class::<io::SeqStats>()?;
    m.add_function(wrap_pyfunction!(io::io_psse_sequence_apply, m)?)?;
    m.add_function(wrap_pyfunction!(io::io_psse_sequence_apply_text, m)?)?;
    m.add_class::<io::DynamicModel>()?;
    m.add_function(wrap_pyfunction!(io::io_psse_dyr_load, m)?)?;
    m.add_function(wrap_pyfunction!(io::io_psse_dyr_loads, m)?)?;
    m.add_function(wrap_pyfunction!(io::io_psse_dyr_save, m)?)?;
    m.add_function(wrap_pyfunction!(io::io_psse_dyr_dumps, m)?)?;
    m.add_function(wrap_pyfunction!(io::ohm_to_pu, m)?)?;
    m.add_class::<pf::LsfResult>()?;
    m.add_function(wrap_pyfunction!(pf::compute_loss_factors, m)?)?;
    m.add_function(wrap_pyfunction!(io::merge_networks, m)?)?;

    // Power flow
    m.add_function(wrap_pyfunction!(pf::solve_dc_pf, m)?)?;
    m.add_function(wrap_pyfunction!(pf::solve_ac_pf, m)?)?;
    m.add_function(wrap_pyfunction!(pf::solve_hvdc, m)?)?;

    // Sensitivity matrices
    m.add_class::<matrices::PreparedDcStudy>()?;
    m.add_class::<matrices::PtdfResult>()?;
    m.add_class::<matrices::LodfResult>()?;
    m.add_class::<matrices::LodfMatrixResult>()?;
    m.add_class::<matrices::N2LodfResult>()?;
    m.add_class::<matrices::N2LodfBatchResult>()?;
    m.add_class::<matrices::OtdfResult>()?;
    m.add_class::<matrices::BldfResult>()?;
    m.add_class::<matrices::GsfResult>()?;
    m.add_class::<matrices::InjectionCapabilityResult>()?;
    m.add_class::<matrices::YBusResult>()?;
    m.add_class::<matrices::JacobianResult>()?;
    m.add_function(wrap_pyfunction!(matrices::compute_ptdf, m)?)?;
    m.add_function(wrap_pyfunction!(matrices::prepare_dc_study, m)?)?;
    m.add_function(wrap_pyfunction!(matrices::compute_lodf, m)?)?;
    m.add_function(wrap_pyfunction!(matrices::compute_lodf_matrix, m)?)?;
    m.add_function(wrap_pyfunction!(matrices::compute_n2_lodf, m)?)?;
    m.add_function(wrap_pyfunction!(matrices::compute_n2_lodf_batch, m)?)?;
    m.add_function(wrap_pyfunction!(matrices::compute_otdf, m)?)?;
    m.add_function(wrap_pyfunction!(matrices::compute_bldf, m)?)?;
    m.add_function(wrap_pyfunction!(matrices::compute_gsf, m)?)?;
    m.add_function(wrap_pyfunction!(matrices::compute_injection_capability, m)?)?;

    // Transfer / ATC
    m.add_class::<transfer::TransferPath>()?;
    m.add_class::<transfer::Flowgate>()?;
    m.add_class::<transfer::AtcOptions>()?;
    m.add_class::<transfer::AcAtcResult>()?;
    m.add_class::<transfer::NercAtcResult>()?;
    m.add_class::<transfer::AfcResult>()?;
    m.add_class::<transfer::MultiTransferResult>()?;
    m.add_class::<transfer::TransferStudy>()?;
    m.add_function(wrap_pyfunction!(transfer::compute_ac_atc, m)?)?;
    m.add_function(wrap_pyfunction!(transfer::compute_nerc_atc, m)?)?;
    m.add_function(wrap_pyfunction!(transfer::compute_afc, m)?)?;
    m.add_function(wrap_pyfunction!(transfer::compute_multi_transfer, m)?)?;
    m.add_function(wrap_pyfunction!(transfer::prepare_transfer_study, m)?)?;

    // OPF
    m.add_function(wrap_pyfunction!(opf::solve_dc_opf_full, m)?)?;
    m.add_function(wrap_pyfunction!(opf::solve_ac_opf, m)?)?;
    m.add_function(wrap_pyfunction!(opf::solve_ac_opf_subproblem, m)?)?;
    m.add_function(wrap_pyfunction!(opf::solve_scopf, m)?)?;
    m.add_function(wrap_pyfunction!(dispatch::solve_dispatch, m)?)?;
    m.add_function(wrap_pyfunction!(dispatch::assess_dispatch_violations, m)?)?;
    m.add_function(wrap_pyfunction!(
        dispatch::read_tamu_activsg_time_series,
        m
    )?)?;

    // GO Competition Challenge 3 adapter
    m.add_class::<go_c3::GoC3Handle>()?;
    m.add_function(wrap_pyfunction!(go_c3::go_c3_load_problem, m)?)?;
    m.add_function(wrap_pyfunction!(go_c3::go_c3_build_network, m)?)?;
    m.add_function(wrap_pyfunction!(go_c3::go_c3_build_request, m)?)?;
    m.add_function(wrap_pyfunction!(go_c3::go_c3_export_solution, m)?)?;
    m.add_function(wrap_pyfunction!(go_c3::go_c3_save_solution, m)?)?;
    m.add_function(wrap_pyfunction!(go_c3::go_c3_build_workflow, m)?)?;

    // Canonical market workflow types (Phase 3).
    m.add_class::<market::PyMarketStage>()?;
    m.add_class::<market::PyMarketWorkflow>()?;
    m.add_function(wrap_pyfunction!(market::market_stage, m)?)?;
    m.add_function(wrap_pyfunction!(market::solve_market_workflow_py, m)?)?;
    // Contingency
    m.add_class::<contingency::ContingencyOptions>()?;
    m.add_class::<contingency::Contingency>()?;
    m.add_class::<contingency::ContingencyStudy>()?;
    m.add_class::<contingency::PreparedCorrectiveDispatchStudy>()?;
    m.add_class::<contingency::CorrectiveAction>()?;
    m.add_class::<contingency::RemedialAction>()?;
    m.add_function(wrap_pyfunction!(contingency::analyze_n1_branch, m)?)?;
    m.add_function(wrap_pyfunction!(contingency::analyze_n2_branch, m)?)?;
    m.add_function(wrap_pyfunction!(contingency::analyze_n1_generator, m)?)?;
    m.add_function(wrap_pyfunction!(
        contingency::generate_breaker_contingencies,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(contingency::apply_ras, m)?)?;
    m.add_function(wrap_pyfunction!(contingency::analyze_contingencies, m)?)?;
    m.add_function(wrap_pyfunction!(contingency::n1_branch_study, m)?)?;
    m.add_function(wrap_pyfunction!(contingency::n1_generator_study, m)?)?;
    m.add_function(wrap_pyfunction!(contingency::n2_branch_study, m)?)?;
    m.add_function(wrap_pyfunction!(
        contingency::prepare_corrective_dispatch_study,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(contingency::solve_corrective_dispatch, m)?)?;
    m.add_function(wrap_pyfunction!(contingency::rank_contingencies, m)?)?;
    m.add_function(wrap_pyfunction!(contingency::analyze_branch_eens, m)?)?;

    // Stability / cascade / FDPF
    m.add_class::<stability::VoltageStressBus>()?;
    m.add_class::<stability::VoltageStressOptions>()?;
    m.add_class::<stability::VoltageStressResult>()?;
    m.add_class::<stability::CascadeOptions>()?;
    m.add_class::<stability::CascadeEvent>()?;
    m.add_class::<stability::CascadeResult>()?;
    m.add_class::<stability::OpaOptions>()?;
    m.add_class::<stability::OpaCascadeResult>()?;
    m.add_function(wrap_pyfunction!(stability::compute_voltage_stress, m)?)?;
    m.add_function(wrap_pyfunction!(stability::solve_fdpf, m)?)?;
    m.add_function(wrap_pyfunction!(stability::simulate_cascade_py, m)?)?;
    m.add_function(wrap_pyfunction!(stability::analyze_cascade_screening, m)?)?;
    m.add_function(wrap_pyfunction!(stability::analyze_opa_cascade_py, m)?)?;

    // Rich objects
    rich_objects::register(m)?;

    // Parameter sweep
    m.add_function(wrap_pyfunction!(parameter_sweep::parameter_sweep, m)?)?;
    m.add_class::<parameter_sweep::SweepResult>()?;
    m.add_class::<parameter_sweep::SweepResults>()?;

    // Built-in test networks
    m.add_function(wrap_pyfunction!(test_networks::case9, m)?)?;
    m.add_function(wrap_pyfunction!(test_networks::case14, m)?)?;
    m.add_function(wrap_pyfunction!(test_networks::case30, m)?)?;
    m.add_function(wrap_pyfunction!(test_networks::market30, m)?)?;
    m.add_function(wrap_pyfunction!(test_networks::case57, m)?)?;
    m.add_function(wrap_pyfunction!(test_networks::case118, m)?)?;
    m.add_function(wrap_pyfunction!(test_networks::case300, m)?)?;

    Ok(())
}
