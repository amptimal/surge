// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Surge HVDC — HVDC power flow modeling for LCC and VSC converters.
//!
//! The stable root surface is intentionally small:
//!
//! - [`solve_hvdc`] for solving HVDC behavior embedded in a [`surge_network::Network`]
//! - [`HvdcOptions`] / [`HvdcMethod`] for selecting the solve strategy
//! - [`HvdcSolution`] / [`HvdcStationSolution`] for the canonical result contract
//! - [`model`] for public HVDC domain types
//! - [`advanced`] and [`experimental`] for lower-level solver access
//!
//! [`solve_hvdc`] auto-routes to the appropriate solver based on network topology
//! and the requested [`HvdcMethod`]:
//!
//! | Method | When chosen by Auto | Description |
//! |--------|---------------------|-------------|
//! | **Sequential** | Point-to-point links only | AC-DC outer iteration (LCC + VSC) |
//! | **BlockCoupled** | VSC-only explicit `dc_grid` | Alternating AC/DC with optional sensitivity correction |
//! | **Hybrid** | Mixed LCC+VSC explicit `dc_grid` | Hybrid NR on DC KCL equations |
//!
//! # Module structure
//!
//! - [`model`] — Data models, control modes, and converter physics (LCC + VSC).
//! - [`advanced`] — Lower-level stable solver entrypoints for advanced workflows.
//! - [`experimental`] — Solver paths not yet promoted to the stable root surface.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use surge_hvdc::{HvdcOptions, solve_hvdc};
//! use surge_network::Network;
//!
//! let network = Network::new("example");
//! let opts = HvdcOptions::default();
//! // let sol = solve_hvdc(&network, &opts).unwrap();
//! ```

pub mod advanced;
mod bridge;
mod dc_network;
mod error;
pub mod experimental;
pub mod interop;
pub mod model;
mod options;
mod result;
mod solver;

pub use error::HvdcError;
pub use model::{
    CommutationCheck, HvdcLink, LccHvdcControlMode, LccHvdcLink, TapControl, VscHvdcControlMode,
    VscHvdcLink, VscStationState, check_commutation_failure,
};
pub use options::HvdcOptions;
pub use result::{
    HvdcDcBusSolution, HvdcLccDetail, HvdcMethod, HvdcSolution, HvdcStationSolution, HvdcTechnology,
};

use surge_network::Network;
use tracing::info;

/// Solve HVDC power flow for a network.
///
/// This is the **unified entry point** for all HVDC solver methods. It
/// auto-detects the appropriate solver based on network topology when
/// `options.method == HvdcMethod::Auto`:
///
/// - **Sequential**: used when the network has only point-to-point HVDC links
///   (`network.hvdc.links`). Converts each link to P/Q
///   injections, runs AC power flow, updates converter operating points, and repeats.
///
/// - **BlockCoupled**: used when the network has one explicit MTDC topology
///   (`network.hvdc.dc_grids[*]`). Alternates AC and DC sub-solves with
///   optional cross-coupling sensitivity corrections.
///
/// # Errors
///
/// Returns [`HvdcError::BusNotFound`] if any converter bus is absent.
/// Returns [`HvdcError::AcPfFailed`] if the AC power flow diverges.
/// Returns [`HvdcError::NotConverged`] if the DC iteration fails to converge.
pub fn solve_hvdc(network: &Network, options: &HvdcOptions) -> Result<HvdcSolution, HvdcError> {
    validate_hvdc_topology(network)?;
    let method = resolve_hvdc_method(network, options.method)?;

    match method {
        HvdcMethod::Auto => unreachable!("resolved above"),
        HvdcMethod::Sequential => {
            let links = bridge::psse::hvdc_links_from_network(network);
            solver::sequential::solve_sequential(network, &links, options)
        }
        HvdcMethod::BlockCoupled => {
            let (mut dc_net, stations) = bridge::mtdc_builder::build_vsc_mtdc_system(network)?;
            let block_opts = to_block_coupled_opts(options);
            let result = solver::block_coupled::solve_block_coupled_ac_dc(
                network,
                &mut dc_net,
                &stations,
                &block_opts,
            )?;
            result.to_hvdc_solution(network)
        }
        HvdcMethod::Hybrid => solver::hybrid_glue::solve_hybrid_ac_dc(network, options),
    }
}

/// Solve point-to-point HVDC power flow with explicitly provided links.
///
/// This is the canonical entrypoint for callers that construct [`HvdcLink`] values
/// manually rather than extracting them from a [`surge_network::Network`]. It
/// always uses the sequential AC-DC iteration method.
pub fn solve_hvdc_links(
    network: &Network,
    links: &[HvdcLink],
    options: &HvdcOptions,
) -> Result<HvdcSolution, HvdcError> {
    validate_hvdc_topology(network)?;
    if network.hvdc.has_explicit_dc_topology() {
        return Err(HvdcError::UnsupportedConfiguration(
            "solve_hvdc_links only supports point-to-point HVDC links; explicit DC-network topology must use solve_hvdc with method='block_coupled' or method='hybrid'"
                .to_string(),
        ));
    }
    solver::sequential::solve_sequential(network, links, options)
}

// ── Conversion helpers ───────────────────────────────────────────────────────

/// Convert canonical [`HvdcOptions`] to block-coupled solver options.
fn to_block_coupled_opts(
    options: &HvdcOptions,
) -> solver::block_coupled::BlockCoupledAcDcSolverOptions {
    solver::block_coupled::BlockCoupledAcDcSolverOptions {
        tol: options.tol,
        max_iter: options.max_iter as usize,
        ac_tol: options.ac_tol,
        ac_max_iter: options.max_ac_iter,
        dc_tol: options.dc_tol,
        dc_max_iter: options.max_dc_iter as usize,
        flat_start: options.flat_start,
        apply_coupling_sensitivities: options.coupling_sensitivities,
        coordinated_droop: options.coordinated_droop,
        ..solver::block_coupled::BlockCoupledAcDcSolverOptions::default()
    }
}

fn explicit_dc_grid_count(network: &Network) -> usize {
    network
        .hvdc
        .dc_grids
        .iter()
        .filter(|grid| !grid.is_empty())
        .count()
}

fn has_explicit_lcc_converters(network: &Network) -> bool {
    network
        .hvdc
        .dc_converters()
        .any(|converter| converter.is_lcc())
}

pub(crate) fn single_explicit_dc_grid(
    network: &Network,
) -> Result<&surge_network::network::DcGrid, HvdcError> {
    let mut grids = network.hvdc.dc_grids.iter().filter(|grid| !grid.is_empty());
    let Some(grid) = grids.next() else {
        return Err(HvdcError::UnsupportedConfiguration(
            "explicit HVDC solves require one canonical dc_grid".to_string(),
        ));
    };
    if grids.next().is_some() {
        return Err(HvdcError::UnsupportedConfiguration(
            "canonical HVDC storage may contain multiple dc_grids, but surge-hvdc currently solves exactly one explicit dc_grid per solve".to_string(),
        ));
    }
    Ok(grid)
}

fn validate_hvdc_topology(network: &Network) -> Result<(), HvdcError> {
    if network.hvdc.has_point_to_point_links() && network.hvdc.has_explicit_dc_topology() {
        return Err(HvdcError::UnsupportedConfiguration(
            "network mixes point-to-point HVDC links with explicit DC-network topology; choose one canonical HVDC representation per solve".to_string(),
        ));
    }
    if explicit_dc_grid_count(network) > 1 {
        return Err(HvdcError::UnsupportedConfiguration(
            "canonical HVDC storage supports multiple explicit dc_grids, but surge-hvdc currently solves one explicit dc_grid at a time".to_string(),
        ));
    }
    Ok(())
}

fn resolve_hvdc_method(network: &Network, requested: HvdcMethod) -> Result<HvdcMethod, HvdcError> {
    let explicit = network.hvdc.has_explicit_dc_topology();
    let point_to_point = network.hvdc.has_point_to_point_links();

    let resolved = match requested {
        HvdcMethod::Auto => {
            if explicit {
                let has_lcc = has_explicit_lcc_converters(network);
                if has_lcc {
                    info!("Auto-routing: explicit mixed LCC/VSC DC topology detected -> Hybrid");
                    HvdcMethod::Hybrid
                } else {
                    info!("Auto-routing: explicit VSC DC topology detected -> BlockCoupled");
                    HvdcMethod::BlockCoupled
                }
            } else {
                if point_to_point {
                    info!("Auto-routing: point-to-point HVDC links detected -> Sequential");
                }
                HvdcMethod::Sequential
            }
        }
        other => other,
    };

    match resolved {
        HvdcMethod::Auto => unreachable!("resolved above"),
        HvdcMethod::Sequential if explicit => Err(HvdcError::UnsupportedConfiguration(
            "HvdcMethod::Sequential only supports point-to-point HVDC links; explicit DC-network topology must use HvdcMethod::BlockCoupled or HvdcMethod::Hybrid".to_string(),
        )),
        HvdcMethod::BlockCoupled if !explicit => Err(HvdcError::UnsupportedConfiguration(
            "HvdcMethod::BlockCoupled requires one explicit dc_grid".to_string(),
        )),
        HvdcMethod::BlockCoupled if has_explicit_lcc_converters(network) => {
            Err(HvdcError::UnsupportedConfiguration(
                "HvdcMethod::BlockCoupled only supports VSC converters; use HvdcMethod::Hybrid for mixed LCC/VSC DC networks".to_string(),
            ))
        }
        HvdcMethod::Hybrid if !explicit => Err(HvdcError::UnsupportedConfiguration(
            "HvdcMethod::Hybrid requires one explicit dc_grid".to_string(),
        )),
        HvdcMethod::Hybrid if !has_explicit_lcc_converters(network) => {
            Err(HvdcError::UnsupportedConfiguration(
                "HvdcMethod::Hybrid requires at least one explicit LCC converter; use HvdcMethod::BlockCoupled for pure-VSC DC networks".to_string(),
            ))
        }
        method => Ok(method),
    }
}

// Hybrid solver glue is in solver/hybrid_glue.rs.

#[cfg(test)]
mod tests {
    use super::{HvdcOptions, solve_hvdc};
    use crate::HvdcError;
    use crate::result::HvdcMethod;
    use crate::solver::hybrid_glue::{
        build_hybrid_mtdc_from_network, hybrid_mtdc_to_hvdc_solution,
    };
    use crate::solver::hybrid_mtdc::{HybridMtdcResult, LccConverterResult, VscConverterResult};
    use surge_network::Network;
    use surge_network::network::{
        Branch, Bus, BusType, DcBranch, DcBus, DcConverter, DcConverterStation, Generator,
        LccDcConverter, LccDcConverterRole, Load,
    };

    fn build_ac_network(name: &str) -> Network {
        let mut network = Network::new(name);
        network.base_mva = 100.0;

        let mut slack = Bus::new(1, BusType::Slack, 230.0);
        slack.voltage_magnitude_pu = 1.01;
        network.buses.push(slack);

        let pq = Bus::new(2, BusType::PQ, 230.0);
        network.buses.push(pq);
        network.loads.push(Load::new(2, 60.0, 15.0));

        network
            .branches
            .push(Branch::new_line(1, 2, 0.01, 0.05, 0.02));

        let mut generator = Generator::new(1, 180.0, 1.01);
        generator.pmax = 500.0;
        generator.qmax = 300.0;
        generator.qmin = -300.0;
        network.generators.push(generator);

        network
    }

    fn add_explicit_dc_topology(network: &mut Network) {
        let grid = network.hvdc.ensure_dc_grid(1, None);
        grid.buses.push(DcBus {
            bus_id: 101,
            p_dc_mw: 0.0,
            v_dc_pu: 1.02,
            base_kv_dc: 320.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.01,
            r_ground_ohm: 400.0,
        });
        grid.buses.push(DcBus {
            bus_id: 202,
            p_dc_mw: 0.0,
            v_dc_pu: 0.99,
            base_kv_dc: 400.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });
        grid.branches.push(DcBranch {
            id: String::new(),
            from_bus: 101,
            to_bus: 202,
            r_ohm: 12.0,
            l_mh: 0.0,
            c_uf: 0.0,
            rating_a_mva: 0.0,
            rating_b_mva: 0.0,
            rating_c_mva: 0.0,
            status: true,
        });
    }

    fn converter_template() -> DcConverterStation {
        DcConverterStation {
            id: String::new(),
            dc_bus: 101,
            ac_bus: 2,
            control_type_dc: 1,
            control_type_ac: 1,
            active_power_mw: 999.0,
            reactive_power_mvar: 0.0,
            is_lcc: false,
            voltage_setpoint_pu: 1.0,
            transformer_r_pu: 0.0,
            transformer_x_pu: 0.0,
            transformer: false,
            tap_ratio: 1.0,
            filter_susceptance_pu: 0.0,
            filter: false,
            reactor_r_pu: 0.0,
            reactor_x_pu: 0.12,
            reactor: true,
            base_kv_ac: 230.0,
            voltage_max_pu: 1.1,
            voltage_min_pu: 0.9,
            current_max_pu: 10.0,
            status: true,
            loss_constant_mw: 1.0,
            loss_linear: 0.1,
            loss_quadratic_rectifier: 0.02,
            loss_quadratic_inverter: 0.03,
            droop: 25.0,
            power_dc_setpoint_mw: 80.0,
            voltage_dc_setpoint_pu: 1.0,
            active_power_ac_max_mw: 100.0,
            active_power_ac_min_mw: -100.0,
            reactive_power_ac_max_mvar: 50.0,
            reactive_power_ac_min_mvar: -50.0,
        }
    }

    fn build_explicit_vsc_network() -> Network {
        let mut network = build_ac_network("explicit-vsc-root");
        add_explicit_dc_topology(&mut network);

        let template = converter_template();
        let grid = network.hvdc.find_dc_grid_mut(1).expect("grid exists");
        grid.converters.push(DcConverter::Vsc(template.clone()));
        grid.converters.push(DcConverter::Vsc(DcConverterStation {
            dc_bus: 202,
            ac_bus: 1,
            control_type_dc: 2,
            power_dc_setpoint_mw: 0.0,
            voltage_dc_setpoint_pu: 1.01,
            ..template
        }));

        network
    }

    fn build_explicit_hybrid_network() -> Network {
        let mut network = build_ac_network("explicit-hybrid-root");
        add_explicit_dc_topology(&mut network);

        let template = converter_template();
        let grid = network.hvdc.find_dc_grid_mut(1).expect("grid exists");
        grid.converters.push(DcConverter::Lcc(LccDcConverter {
            id: String::new(),
            dc_bus: template.dc_bus,
            ac_bus: template.ac_bus,
            n_bridges: 1,
            alpha_max_deg: 90.0,
            alpha_min_deg: 5.0,
            gamma_min_deg: 15.0,
            commutation_resistance_ohm: 0.0,
            commutation_reactance_ohm: template.reactor_x_pu
                * template.base_kv_ac
                * template.base_kv_ac
                / network.base_mva,
            base_voltage_kv: template.base_kv_ac,
            turns_ratio: 1.0,
            tap_ratio: 1.0,
            tap_max: 1.1,
            tap_min: 0.9,
            tap_step: 0.00625,
            scheduled_setpoint: template.power_dc_setpoint_mw,
            power_share_percent: 0.0,
            current_margin_percent: 0.0,
            role: LccDcConverterRole::Rectifier,
            in_service: true,
        }));
        grid.converters.push(DcConverter::Vsc(DcConverterStation {
            dc_bus: 202,
            ac_bus: 1,
            control_type_dc: 2,
            is_lcc: false,
            active_power_mw: -888.0,
            reactive_power_mvar: 5.0,
            power_dc_setpoint_mw: -80.0,
            voltage_dc_setpoint_pu: 1.01,
            ..template
        }));

        network
    }

    #[test]
    fn explicit_hybrid_builder_uses_canonical_dc_fields_and_bus_physics() {
        let mut network = build_explicit_hybrid_network();
        network.hvdc.dc_grids[0].converters[0]
            .as_lcc_mut()
            .expect("lcc converter")
            .scheduled_setpoint = 120.0;

        let hybrid = build_hybrid_mtdc_from_network(&network).expect("builder should succeed");

        assert_eq!(hybrid.lcc_converters[0].p_setpoint_mw, 120.0);
        assert_eq!(hybrid.vsc_converters[0].p_setpoint_mw, -80.0);
        assert!((hybrid.lcc_converters[0].x_commutation_pu - 0.12).abs() < 1e-12);
        assert!((hybrid.vsc_converters[0].loss_quadratic_rectifier - 0.02).abs() < 1e-12);
        assert!((hybrid.vsc_converters[0].loss_quadratic_inverter - 0.03).abs() < 1e-12);
        assert_eq!(hybrid.dc_network.slack_dc_bus, 1);
        assert_eq!(hybrid.dc_network.v_dc_slack, 1.01);

        let z_branch = 360.0_f64 * 360.0 / 100.0;
        assert!((hybrid.dc_network.branches[0].r_dc_pu - 12.0 / z_branch).abs() < 1e-12);

        let z_bus0 = 320.0_f64 * 320.0 / 100.0;
        assert!((hybrid.dc_network.g_shunt_pu[0] - 0.01 * z_bus0).abs() < 1e-12);
        assert!((hybrid.dc_network.g_ground_pu[0] - z_bus0 / 400.0).abs() < 1e-12);
        assert!((hybrid.dc_network.v_dc[0] - 1.02).abs() < 1e-12);
    }

    #[test]
    fn hybrid_solution_preserves_signed_dc_power() {
        let network = build_ac_network("hybrid-solution");
        let solution = hybrid_mtdc_to_hvdc_solution(
            &network,
            &HybridMtdcResult {
                dc_voltages_pu: vec![1.0],
                lcc_results: vec![LccConverterResult {
                    bus_ac: 1,
                    p_ac_mw: -102.0,
                    q_ac_mvar: -20.0,
                    p_dc_mw: 100.0,
                    v_dc_pu: 1.0,
                    i_dc_pu: 1.0,
                    alpha_deg: 15.0,
                    gamma_deg: 18.0,
                    power_factor: 0.95,
                }],
                vsc_results: vec![VscConverterResult {
                    bus_ac: 2,
                    p_ac_mw: 98.0,
                    q_ac_mvar: 5.0,
                    p_dc_mw: -100.0,
                    v_dc_pu: 0.99,
                    i_dc_pu: -1.0,
                    losses_mw: 2.0,
                }],
                total_dc_loss_mw: 1.5,
                converged: true,
                iterations: 3,
            },
        );

        assert_eq!(solution.stations[0].p_dc_mw, 100.0);
        assert_eq!(solution.stations[1].p_dc_mw, -100.0);
        assert!((solution.total_loss_mw - 5.5).abs() < 1e-12);
    }

    #[test]
    fn solve_auto_uses_block_coupled_for_explicit_vsc_topology() {
        let network = build_explicit_vsc_network();

        let auto =
            solve_hvdc(&network, &HvdcOptions::default()).expect("auto solve should succeed");
        let explicit = solve_hvdc(
            &network,
            &HvdcOptions {
                method: HvdcMethod::BlockCoupled,
                ..HvdcOptions::default()
            },
        )
        .expect("explicit block-coupled solve should succeed");

        assert!(
            auto.converged,
            "auto-routed block-coupled solve should converge"
        );
        assert_eq!(auto.method, HvdcMethod::BlockCoupled);
        assert_eq!(auto.stations.len(), 2);
        assert_eq!(auto.dc_buses.len(), 2);
        assert!((auto.total_loss_mw - explicit.total_loss_mw).abs() < 1e-9);
        for (lhs, rhs) in auto.dc_buses.iter().zip(explicit.dc_buses.iter()) {
            assert!((lhs.voltage_pu - rhs.voltage_pu).abs() < 1e-9);
        }
    }

    #[test]
    fn solve_auto_uses_hybrid_for_mixed_explicit_topology() {
        let network = build_explicit_hybrid_network();

        let auto =
            solve_hvdc(&network, &HvdcOptions::default()).expect("auto solve should succeed");
        let explicit = solve_hvdc(
            &network,
            &HvdcOptions {
                method: HvdcMethod::Hybrid,
                flat_start: false,
                ..HvdcOptions::default()
            },
        )
        .expect("explicit hybrid solve should succeed");

        assert!(
            auto.converged,
            "auto-routed hybrid solve should converge on the test topology"
        );
        assert_eq!(auto.method, HvdcMethod::Hybrid);
        assert_eq!(auto.stations.len(), 2);
        assert_eq!(auto.dc_buses.len(), 2);
        assert!(
            auto.stations
                .iter()
                .any(|station| station.lcc_detail.is_some())
        );
        assert!(
            auto.stations
                .iter()
                .any(|station| station.lcc_detail.is_none())
        );
        for (lhs, rhs) in auto.dc_buses.iter().zip(explicit.dc_buses.iter()) {
            assert!((lhs.voltage_pu - rhs.voltage_pu).abs() < 1e-9);
        }
    }

    #[test]
    fn explicit_hybrid_builder_requires_exactly_one_vsc_slack_converter() {
        let mut network = build_explicit_hybrid_network();
        network.hvdc.dc_grids[0].converters[1]
            .as_vsc_mut()
            .expect("vsc converter")
            .control_type_dc = 1;

        assert!(matches!(
            build_hybrid_mtdc_from_network(&network),
            Err(HvdcError::UnsupportedConfiguration(message))
                if message.contains("exactly one VSC converter with DC-voltage control")
        ));
    }

    #[test]
    fn explicit_hybrid_builder_rejects_unknown_converter_dc_bus() {
        let mut network = build_explicit_hybrid_network();
        *network.hvdc.dc_grids[0].converters[0].dc_bus_mut() = 999;

        assert!(matches!(
            build_hybrid_mtdc_from_network(&network),
            Err(HvdcError::InvalidLink(message)) if message.contains("dc_bus 999")
        ));
    }

    #[test]
    fn explicit_hybrid_builder_rejects_nonpositive_dc_base_kv() {
        let mut network = build_explicit_hybrid_network();
        network.hvdc.dc_grids[0].buses[0].base_kv_dc = 0.0;

        assert!(matches!(
            build_hybrid_mtdc_from_network(&network),
            Err(HvdcError::UnsupportedConfiguration(message))
                if message.contains("base_kv_dc")
        ));
    }

    #[test]
    fn block_coupled_option_conversion_preserves_flat_start() {
        let opts = HvdcOptions {
            flat_start: false,
            ..HvdcOptions::default()
        };
        let block_opts = super::to_block_coupled_opts(&opts);
        assert!(!block_opts.flat_start);

        let default_block_opts = super::to_block_coupled_opts(&HvdcOptions::default());
        assert!(default_block_opts.flat_start);
    }
}
