// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Network mutation methods for the Python `Network` class.
//!
//! All methods call `Arc::make_mut(&mut self.inner)` to obtain a `&mut Network`
//! via copy-on-write semantics — no `RwLock` required, all solver code unchanged.

use std::collections::HashMap;
use std::sync::Arc;

use num_complex::Complex64;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use surge_network::market::CombinedCycleConfig as CoreCombinedCycleConfig;
use surge_network::market::CombinedCyclePlant as CoreCombinedCyclePlant;
use surge_network::market::CombinedCycleTransition as CoreCombinedCycleTransition;
use surge_network::market::CostCurve;
use surge_network::market::DispatchableLoad as CoreDispatchableLoad;
use surge_network::market::LoadArchetype;
use surge_network::market::LoadCostModel;
use surge_network::market::PumpedHydroUnit as CorePumpedHydroUnit;
use surge_network::network::AreaSchedule as CoreAreaSchedule;
use surge_network::network::Branch;
use surge_network::network::BranchOpfControl;
use surge_network::network::BranchRef;
use surge_network::network::CommitmentParams;
use surge_network::network::CommitmentStatus;
use surge_network::network::FuelParams;
use surge_network::network::GenFaultData;
use surge_network::network::Generator;
use surge_network::network::GeneratorRef;
use surge_network::network::HarmonicData;
use surge_network::network::LccHvdcLink;
use surge_network::network::Load;
use surge_network::network::MarketParams;
use surge_network::network::PhaseMode;
use surge_network::network::RampingParams;
use surge_network::network::ReactiveCapability;
use surge_network::network::TapMode;
use surge_network::network::TransformerConnection;
use surge_network::network::TransformerData;
use surge_network::network::WeightedBranchRef;
use surge_network::network::ZeroSeqData;
use surge_network::network::breaker::BreakerRating as CoreBreakerRating;
use surge_network::network::{Bus, BusType};
use surge_network::network::{FactsDevice, FactsMode};
use surge_network::network::{FixedShunt as CoreFixedShunt, ShuntType};
use surge_network::network::{VscConverterAcControlMode, VscHvdcControlMode, VscHvdcLink};

use crate::Network;
use crate::exceptions::NetworkError;
use crate::input_types::PyBranchKey;
use crate::rich_objects;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn parse_facts_mode(s: &str) -> PyResult<FactsMode> {
    match s {
        "OutOfService" => Ok(FactsMode::OutOfService),
        "SeriesOnly" => Ok(FactsMode::SeriesOnly),
        "ShuntOnly" => Ok(FactsMode::ShuntOnly),
        "ShuntSeries" => Ok(FactsMode::ShuntSeries),
        "SeriesPowerControl" => Ok(FactsMode::SeriesPowerControl),
        "ImpedanceModulation" => Ok(FactsMode::ImpedanceModulation),
        _ => Err(PyValueError::new_err(format!(
            "FACTS mode must be one of 'OutOfService', 'SeriesOnly', 'ShuntOnly', \
             'ShuntSeries', 'SeriesPowerControl', 'ImpedanceModulation'; got '{s}'"
        ))),
    }
}

fn parse_bus_type(s: &str) -> PyResult<BusType> {
    match s {
        "PQ" => Ok(BusType::PQ),
        "PV" => Ok(BusType::PV),
        "Slack" => Ok(BusType::Slack),
        "Isolated" => Ok(BusType::Isolated),
        _ => Err(PyValueError::new_err(format!(
            "bus_type must be 'PQ', 'PV', 'Slack', or 'Isolated', got '{s}'"
        ))),
    }
}

fn parse_commitment_status(s: &str) -> PyResult<CommitmentStatus> {
    match s {
        "Market" => Ok(CommitmentStatus::Market),
        "SelfCommitted" => Ok(CommitmentStatus::SelfCommitted),
        "MustRun" => Ok(CommitmentStatus::MustRun),
        "Unavailable" => Ok(CommitmentStatus::Unavailable),
        "EmergencyOnly" => Ok(CommitmentStatus::EmergencyOnly),
        _ => Err(PyValueError::new_err(format!(
            "commitment_status must be one of 'Market', 'SelfCommitted', 'MustRun', 'Unavailable', 'EmergencyOnly'; got '{s}'"
        ))),
    }
}

fn parse_transformer_connection(s: &str) -> PyResult<TransformerConnection> {
    match s {
        "WyeG-WyeG" => Ok(TransformerConnection::WyeGWyeG),
        "WyeG-Delta" => Ok(TransformerConnection::WyeGDelta),
        "Delta-WyeG" => Ok(TransformerConnection::DeltaWyeG),
        "Delta-Delta" => Ok(TransformerConnection::DeltaDelta),
        "WyeG-Wye" => Ok(TransformerConnection::WyeGWye),
        _ => Err(PyValueError::new_err(format!(
            "transformer_connection must be one of 'WyeG-WyeG', 'WyeG-Delta', 'Delta-WyeG', 'Delta-Delta', 'WyeG-Wye'; got '{s}'"
        ))),
    }
}

fn parse_tap_mode(s: &str) -> PyResult<TapMode> {
    match s {
        "Fixed" => Ok(TapMode::Fixed),
        "Continuous" => Ok(TapMode::Continuous),
        _ => Err(PyValueError::new_err(format!(
            "tap_mode must be 'Fixed' or 'Continuous', got '{s}'"
        ))),
    }
}

fn parse_phase_mode(s: &str) -> PyResult<PhaseMode> {
    match s {
        "Fixed" => Ok(PhaseMode::Fixed),
        "Continuous" => Ok(PhaseMode::Continuous),
        _ => Err(PyValueError::new_err(format!(
            "phase_mode must be 'Fixed' or 'Continuous', got '{s}'"
        ))),
    }
}

fn parse_shunt_type(s: &str) -> PyResult<ShuntType> {
    match s {
        "Capacitor" => Ok(ShuntType::Capacitor),
        "Reactor" => Ok(ShuntType::Reactor),
        "HarmonicFilter" => Ok(ShuntType::HarmonicFilter),
        _ => Err(PyValueError::new_err(format!(
            "shunt_type must be one of 'Capacitor', 'Reactor', 'HarmonicFilter'; got '{s}'"
        ))),
    }
}

fn parse_load_archetype(s: &str) -> PyResult<LoadArchetype> {
    match s {
        "Curtailable" => Ok(LoadArchetype::Curtailable),
        "Interruptible" => Ok(LoadArchetype::Interruptible),
        _ => Err(PyValueError::new_err(format!(
            "dispatchable load archetype must be 'Curtailable' or 'Interruptible'; got '{s}'"
        ))),
    }
}

fn build_generator_cost_curve(generator: &rich_objects::Generator) -> PyResult<Option<CostCurve>> {
    match generator.cost_model.as_deref() {
        None => Ok(None),
        Some("polynomial") => Ok(Some(CostCurve::Polynomial {
            startup: generator.cost_startup,
            shutdown: generator.cost_shutdown,
            coeffs: generator.cost_coefficients.clone(),
        })),
        Some("piecewise_linear") => {
            if generator.cost_breakpoints_mw.len() != generator.cost_breakpoints_usd.len() {
                return Err(PyValueError::new_err(
                    "piecewise-linear generator cost breakpoints must have matching MW and USD lengths",
                ));
            }
            Ok(Some(CostCurve::PiecewiseLinear {
                startup: generator.cost_startup,
                shutdown: generator.cost_shutdown,
                points: generator
                    .cost_breakpoints_mw
                    .iter()
                    .copied()
                    .zip(generator.cost_breakpoints_usd.iter().copied())
                    .collect(),
            }))
        }
        Some(other) => Err(PyValueError::new_err(format!(
            "generator cost_model must be 'polynomial', 'piecewise_linear', or None; got '{other}'"
        ))),
    }
}

fn dispatchable_load_cost_model(
    archetype: LoadArchetype,
    cost_per_mwh: Option<f64>,
) -> LoadCostModel {
    let cost_per_mwh = cost_per_mwh.unwrap_or(0.0);
    match archetype {
        LoadArchetype::Interruptible => LoadCostModel::InterruptPenalty {
            cost_per_mw: cost_per_mwh,
        },
        _ => LoadCostModel::LinearCurtailment {
            cost_per_mw: cost_per_mwh,
        },
    }
}

fn apply_dispatchable_load_object(
    load: &rich_objects::DispatchableLoad,
    net: &mut surge_network::Network,
    index: usize,
) -> PyResult<()> {
    let archetype = parse_load_archetype(&load.archetype)?;
    if !net.buses.iter().any(|bus| bus.number == load.bus) {
        return Err(NetworkError::new_err(format!("Bus {} not found", load.bus)));
    }
    let base_mva = net.base_mva;
    let target = net
        .market_data
        .dispatchable_loads
        .get_mut(index)
        .ok_or_else(|| {
            NetworkError::new_err(format!("Dispatchable load index {index} out of range"))
        })?;
    target.bus = load.bus;
    target.p_sched_pu = load.p_sched_mw / base_mva;
    target.q_sched_pu = load.q_sched_mvar / base_mva;
    target.p_min_pu = load.pmin_mw / base_mva;
    target.p_max_pu = load.pmax_mw / base_mva;
    target.q_min_pu = load.qmin_mvar / base_mva;
    target.q_max_pu = load.qmax_mvar / base_mva;
    target.archetype = archetype;
    target.cost_model = dispatchable_load_cost_model(archetype, load.cost_per_mwh);
    target.fixed_power_factor = load.fixed_power_factor;
    target.in_service = load.in_service;
    target.product_type = load.product_type.clone();
    target.baseline_mw = load.baseline_mw;
    target.reserve_offers = load
        .reserve_offers
        .iter()
        .map(
            |(product_id, capacity_mw, cost_per_mwh)| surge_network::market::ReserveOffer {
                product_id: product_id.clone(),
                capacity_mw: *capacity_mw,
                cost_per_mwh: *cost_per_mwh,
            },
        )
        .collect();
    target.qualifications = load.qualifications.clone();
    Ok(())
}

fn parse_dispatchable_load_archetype(
    s: &str,
    bus: u32,
    p_sched_mw: f64,
    q_sched_mvar: f64,
    p_min_mw: f64,
    cost_per_mwh: f64,
    base_mva: f64,
) -> PyResult<CoreDispatchableLoad> {
    match s.to_lowercase().as_str() {
        "curtailable" => Ok(CoreDispatchableLoad::curtailable(
            bus,
            p_sched_mw,
            q_sched_mvar,
            p_min_mw,
            cost_per_mwh,
            base_mva,
        )),
        "interruptible" => Ok(CoreDispatchableLoad::interruptible(
            bus,
            p_sched_mw,
            q_sched_mvar,
            cost_per_mwh,
            base_mva,
        )),
        other => Err(PyValueError::new_err(format!(
            "dispatchable load archetype must be 'Curtailable' or 'Interruptible', got '{other}'"
        ))),
    }
}

fn find_generator_mut_by_id<'a>(
    generators: &'a mut [Generator],
    id: &str,
) -> PyResult<&'a mut Generator> {
    generators
        .iter_mut()
        .find(|generator| generator.id == id)
        .ok_or_else(|| NetworkError::new_err(format!("Generator id='{id}' not found")))
}

/// Find a branch index by (from_bus, to_bus, circuit string).
fn find_branch_str(
    branches: &[Branch],
    from_bus: u32,
    to_bus: u32,
    circuit: &str,
) -> Option<usize> {
    branches.iter().position(|br| {
        br.circuit == circuit
            && ((br.from_bus == from_bus && br.to_bus == to_bus)
                || (br.from_bus == to_bus && br.to_bus == from_bus))
    })
}

// ---------------------------------------------------------------------------
// Bus operations
// ---------------------------------------------------------------------------

#[pymethods]
impl Network {
    /// Add a new bus to the network.
    ///
    /// Args:
    ///   number:    External bus number (must be unique).
    ///   bus_type:  ``"PQ"``, ``"PV"``, ``"Slack"``, or ``"Isolated"``.
    ///   base_kv:   Nominal voltage in kV.
    ///   name:      Optional bus name.
    ///   pd_mw:     Active power demand (MW).
    ///   qd_mvar:   Reactive power demand (MVAr).
    ///   vm_pu:     Initial voltage magnitude (p.u.).
    ///   va_deg:    Initial voltage angle (degrees).
    ///
    /// Raises:
    ///   ValueError: if ``bus_type`` is invalid or ``number`` already exists.
    #[pyo3(signature = (number, bus_type, base_kv, name="", pd_mw=0.0, qd_mvar=0.0, vm_pu=1.0, va_deg=0.0))]
    pub fn add_bus(
        &mut self,
        number: u32,
        bus_type: &str,
        base_kv: f64,
        name: &str,
        pd_mw: f64,
        qd_mvar: f64,
        vm_pu: f64,
        va_deg: f64,
    ) -> PyResult<()> {
        let bt = parse_bus_type(bus_type)?;
        let net = Arc::make_mut(&mut self.inner);
        if net.buses.iter().any(|b| b.number == number) {
            return Err(NetworkError::new_err(format!(
                "Bus {number} already exists"
            )));
        }
        let mut bus = Bus::new(number, bt, base_kv);
        bus.name = name.to_string();
        bus.voltage_magnitude_pu = vm_pu;
        bus.voltage_angle_rad = va_deg.to_radians();
        net.buses.push(bus);
        if pd_mw != 0.0 || qd_mvar != 0.0 {
            net.loads.push(Load {
                bus: number,
                id: "1".to_string(),
                in_service: true,
                conforming: true,
                active_power_demand_mw: pd_mw,
                reactive_power_demand_mvar: qd_mvar,
                ..Default::default()
            });
        }
        Ok(())
    }

    /// Remove a bus and all connected branches, generators, and loads.
    ///
    /// Raises:
    ///   ValueError: if the bus does not exist.
    pub fn remove_bus(&mut self, number: u32) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let before = net.buses.len();
        net.buses.retain(|b| b.number != number);
        if net.buses.len() == before {
            return Err(NetworkError::new_err(format!("Bus {number} not found")));
        }
        // Cascade: remove all elements that reference this bus.
        net.branches
            .retain(|br| br.from_bus != number && br.to_bus != number);
        net.generators.retain(|g| g.bus != number);
        net.loads.retain(|l| l.bus != number);
        net.hvdc.links.retain(|link| match link {
            surge_network::network::HvdcLink::Lcc(dc) => {
                dc.rectifier.bus != number && dc.inverter.bus != number
            }
            surge_network::network::HvdcLink::Vsc(vsc) => {
                vsc.converter1.bus != number && vsc.converter2.bus != number
            }
        });
        for grid in &mut net.hvdc.dc_grids {
            grid.converters
                .retain(|converter| converter.ac_bus() != number);
        }
        net.hvdc.dc_grids.retain(|grid| !grid.is_empty());
        net.facts_devices
            .retain(|f| f.bus_from != number && f.bus_to != number);
        // oltc_specs and par_specs use external bus numbers — straightforward.
        net.controls
            .oltc_specs
            .retain(|s| s.from_bus != number && s.to_bus != number);
        net.controls
            .par_specs
            .retain(|s| s.from_bus != number && s.to_bus != number);
        // switched_shunts and switched_shunts_opf use 0-based indices which
        // would also need to be renumbered — not handled here since remove_bus
        // is intended for structural edits before the first solve.
        Ok(())
    }

    /// Set the bus type.
    ///
    /// Args:
    ///   bus:      External bus number.
    ///   bus_type: ``"PQ"``, ``"PV"``, ``"Slack"``, or ``"Isolated"``.
    ///
    /// Raises:
    ///   ValueError: if the bus is not found or bus_type is invalid.
    pub fn set_bus_type(&mut self, bus: u32, bus_type: &str) -> PyResult<()> {
        let bt = parse_bus_type(bus_type)?;
        let net = Arc::make_mut(&mut self.inner);
        net.buses
            .iter_mut()
            .find(|b| b.number == bus)
            .ok_or_else(|| NetworkError::new_err(format!("Bus {bus} not found")))?
            .bus_type = bt;
        Ok(())
    }

    /// Canonicalize runtime-facing identities after topology/service edits.
    ///
    /// This demotes stale PV buses without active regulating generators,
    /// canonicalizes branch circuit ids, and ensures runtime ids stay
    /// consistent after editing in-service equipment.
    pub fn canonicalize_runtime_identities(&mut self) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        net.canonicalize_runtime_identities();
        Ok(())
    }

    /// Set the load at a bus.
    ///
    /// Updates Load objects at this bus. If no Load exists, one is created.
    ///
    /// Raises:
    ///   ValueError: if the bus is not found.
    #[pyo3(signature = (bus, pd_mw, qd_mvar=0.0))]
    pub fn set_bus_load(&mut self, bus: u32, pd_mw: f64, qd_mvar: f64) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        if !net.buses.iter().any(|b| b.number == bus) {
            return Err(NetworkError::new_err(format!("Bus {bus} not found")));
        }
        // Find the first Load at this bus, or create one.
        if let Some(load) = net.loads.iter_mut().find(|l| l.bus == bus) {
            load.active_power_demand_mw = pd_mw;
            load.reactive_power_demand_mvar = qd_mvar;
        } else {
            net.loads.push(Load {
                bus,
                id: "1".to_string(),
                in_service: true,
                conforming: true,
                active_power_demand_mw: pd_mw,
                reactive_power_demand_mvar: qd_mvar,
                ..Default::default()
            });
        }
        Ok(())
    }

    /// Set the voltage setpoint at a bus.
    ///
    /// Raises:
    ///   ValueError: if the bus is not found.
    #[pyo3(signature = (bus, vm_pu, va_deg=0.0))]
    pub fn set_bus_voltage(&mut self, bus: u32, vm_pu: f64, va_deg: f64) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let b = net
            .buses
            .iter_mut()
            .find(|b| b.number == bus)
            .ok_or_else(|| NetworkError::new_err(format!("Bus {bus} not found")))?;
        b.voltage_magnitude_pu = vm_pu;
        b.voltage_angle_rad = va_deg.to_radians();
        Ok(())
    }

    /// Set the shunt admittance at a bus.
    ///
    /// Args:
    ///   bus:    External bus number.
    ///   gs_mw:  Shunt conductance (MW at 1.0 p.u. voltage).
    ///   bs_mvar: Shunt susceptance (MVAr at 1.0 p.u. voltage).
    ///
    /// Raises:
    ///   ValueError: if the bus is not found.
    pub fn set_bus_shunt(&mut self, bus: u32, gs_mw: f64, bs_mvar: f64) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let b = net
            .buses
            .iter_mut()
            .find(|b| b.number == bus)
            .ok_or_else(|| NetworkError::new_err(format!("Bus {bus} not found")))?;
        b.shunt_conductance_mw = gs_mw;
        b.shunt_susceptance_mvar = bs_mvar;
        Ok(())
    }

    /// Add a bus from an editable ``Bus`` object.
    pub fn add_bus_object(&mut self, bus: PyRef<'_, rich_objects::Bus>) -> PyResult<()> {
        self.add_bus(
            bus.number,
            &bus.type_str,
            bus.base_kv,
            &bus.name,
            bus.pd_mw,
            bus.qd_mvar,
            bus.vm_pu,
            bus.va_deg,
        )?;
        self.update_bus_object(bus)
    }

    /// Apply the fields of an editable ``Bus`` object back onto the network.
    pub fn update_bus_object(&mut self, bus: PyRef<'_, rich_objects::Bus>) -> PyResult<()> {
        let bus_type = parse_bus_type(&bus.type_str)?;
        let net = Arc::make_mut(&mut self.inner);
        let target = net
            .buses
            .iter_mut()
            .find(|existing| existing.number == bus.number)
            .ok_or_else(|| NetworkError::new_err(format!("Bus {} not found", bus.number)))?;
        target.name = bus.name.clone();
        target.bus_type = bus_type;
        target.shunt_conductance_mw = bus.gs_mw;
        target.shunt_susceptance_mvar = bus.bs_mvar;
        target.area = bus.area;
        target.zone = bus.zone;
        target.voltage_magnitude_pu = bus.vm_pu;
        target.voltage_angle_rad = bus.va_deg.to_radians();
        target.base_kv = bus.base_kv;
        target.voltage_min_pu = bus.vmin_pu;
        target.voltage_max_pu = bus.vmax_pu;
        target.latitude = bus.latitude;
        target.longitude = bus.longitude;
        // Update load demand via Load objects
        let bus_number = bus.number;
        let pd = bus.pd_mw;
        let qd = bus.qd_mvar;
        if let Some(load) = net.loads.iter_mut().find(|l| l.bus == bus_number) {
            load.active_power_demand_mw = pd;
            load.reactive_power_demand_mvar = qd;
        } else if pd != 0.0 || qd != 0.0 {
            net.loads.push(Load {
                bus: bus_number,
                id: "1".to_string(),
                in_service: true,
                conforming: true,
                active_power_demand_mw: pd,
                reactive_power_demand_mvar: qd,
                ..Default::default()
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Branch operations
// ---------------------------------------------------------------------------

#[pymethods]
impl Network {
    /// Add a new branch (transmission line or transformer).
    ///
    /// Args:
    ///   from_bus:           From-bus number.
    ///   to_bus:             To-bus number.
    ///   r:                  Series resistance (p.u.).
    ///   x:                  Series reactance (p.u.).
    ///   b:                  Total line charging susceptance (p.u.).
    ///   rate_a_mva:         Long-term thermal rating (MVA). 0 = unconstrained.
    ///   tap:                Off-nominal turns ratio (1.0 for lines).
    ///   shift_deg:          Phase shift in degrees (0.0 for lines).
    ///   circuit:            Circuit identifier for parallel lines (default 1).
    ///   skin_effect_alpha:  IEC 60287 skin-effect coefficient for harmonic analysis.
    ///                       If > 0: ``R_h = R × (1 + alpha × (h − 1))``.
    ///                       If 0 (default): ``R_h = R × sqrt(h)``.
    ///   delta_connected:    If ``True``, this branch blocks triplen harmonics
    ///                       (3rd, 6th, 9th, ...) in harmonic analysis.
    ///
    /// Raises:
    ///   ValueError: if from_bus or to_bus is not in the network.
    #[pyo3(signature = (from_bus, to_bus, r, x, b=0.0, rate_a_mva=0.0, tap=1.0, shift_deg=0.0, circuit=1, skin_effect_alpha=0.0, delta_connected=false))]
    pub fn add_branch(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        r: f64,
        x: f64,
        b: f64,
        rate_a_mva: f64,
        tap: f64,
        shift_deg: f64,
        circuit: i64,
        skin_effect_alpha: f64,
        delta_connected: bool,
    ) -> PyResult<()> {
        if from_bus == to_bus {
            return Err(PyValueError::new_err(format!(
                "Cannot add self-loop branch: from_bus and to_bus are both {from_bus}. \
                 A branch must connect two different buses."
            )));
        }
        let net = Arc::make_mut(&mut self.inner);
        if !net.buses.iter().any(|bus| bus.number == from_bus) {
            return Err(NetworkError::new_err(format!(
                "from_bus {from_bus} not found in network"
            )));
        }
        if !net.buses.iter().any(|bus| bus.number == to_bus) {
            return Err(NetworkError::new_err(format!(
                "to_bus {to_bus} not found in network"
            )));
        }
        let mut br = Branch::new_line(from_bus, to_bus, r, x, b);
        br.rating_a_mva = rate_a_mva;
        br.circuit = circuit.to_string();
        br.tap = tap;
        br.phase_shift_rad = shift_deg.to_radians();
        // Match the MATPOWER reader's convention: an off-nominal tap
        // (|tap - 1| > 1e-6) or a non-zero phase shift flags this
        // branch as a transformer. Keeps semantics consistent whether
        // the user loads via MATPOWER or builds networks from Python
        // (including the PyPSA netCDF bridge).
        br.branch_type = if (tap - 1.0).abs() > 1e-6 || shift_deg.abs() > 1e-6 {
            surge_network::network::BranchType::Transformer
        } else {
            surge_network::network::BranchType::Line
        };
        if skin_effect_alpha != 0.0 {
            br.harmonic
                .get_or_insert_with(HarmonicData::default)
                .skin_effect_alpha = skin_effect_alpha;
        }
        if delta_connected {
            br.zero_seq
                .get_or_insert_with(ZeroSeqData::default)
                .delta_connected = delta_connected;
        }
        net.branches.push(br);
        Ok(())
    }

    /// Remove a branch.
    ///
    /// Raises:
    ///   ValueError: if no matching branch is found.
    #[pyo3(signature = (from_bus, to_bus, circuit=1))]
    pub fn remove_branch(&mut self, from_bus: u32, to_bus: u32, circuit: i64) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let c = circuit.to_string();
        let idx = find_branch_str(&net.branches, from_bus, to_bus, &c).ok_or_else(|| {
            NetworkError::new_err(format!("Branch {from_bus}-{to_bus} circuit {c} not found"))
        })?;
        net.branches.remove(idx);
        Ok(())
    }

    /// Set a branch in- or out-of-service (outage simulation).
    ///
    /// Raises:
    ///   ValueError: if no matching branch is found.
    #[pyo3(signature = (from_bus, to_bus, in_service, circuit=1))]
    pub fn set_branch_in_service(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        in_service: bool,
        circuit: i64,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let c = circuit.to_string();
        let idx = find_branch_str(&net.branches, from_bus, to_bus, &c).ok_or_else(|| {
            NetworkError::new_err(format!("Branch {from_bus}-{to_bus} circuit {c} not found"))
        })?;
        net.branches[idx].in_service = in_service;
        Ok(())
    }

    /// Set the tap ratio of a transformer branch.
    ///
    /// Raises:
    ///   ValueError: if no matching branch is found.
    #[pyo3(signature = (from_bus, to_bus, tap, circuit=1))]
    pub fn set_branch_tap(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        tap: f64,
        circuit: i64,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let c = circuit.to_string();
        let idx = find_branch_str(&net.branches, from_bus, to_bus, &c).ok_or_else(|| {
            NetworkError::new_err(format!("Branch {from_bus}-{to_bus} circuit {c} not found"))
        })?;
        net.branches[idx].tap = tap;
        Ok(())
    }

    /// Set the phase-shift angle of a transformer branch (degrees).
    ///
    /// Raises:
    ///   ValueError: if no matching branch is found.
    #[pyo3(signature = (from_bus, to_bus, shift_deg, circuit=1))]
    pub fn set_branch_phase_shift(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        shift_deg: f64,
        circuit: i64,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let c = circuit.to_string();
        let idx = find_branch_str(&net.branches, from_bus, to_bus, &c).ok_or_else(|| {
            NetworkError::new_err(format!("Branch {from_bus}-{to_bus} circuit {c} not found"))
        })?;
        net.branches[idx].phase_shift_rad = shift_deg.to_radians();
        Ok(())
    }

    /// Add a transmission line from physical (engineering) parameters.
    ///
    /// Converts conductor parameters in Ω/km and µS/km to per-unit internally
    /// using the standard base impedance formula: ``z_base = base_kv² / base_mva``.
    ///
    /// Args:
    ///   from_bus:       From-bus number.
    ///   to_bus:         To-bus number.
    ///   r_ohm_per_km:   Series resistance (Ω/km).
    ///   x_ohm_per_km:   Series reactance (Ω/km).
    ///   b_us_per_km:    Shunt susceptance (µS/km — micro-Siemens per km).
    ///   length_km:      Line length in km.
    ///   base_kv:        Nominal voltage (kV) — for per-unit conversion.
    ///   rate_a_mva:     Thermal limit (MVA). 0 = unconstrained.
    ///   circuit:        Circuit identifier for parallel lines (default 1).
    ///
    /// Raises:
    ///   ValueError: if from_bus or to_bus is not in the network, or if
    ///     length_km <= 0 or base_kv <= 0.
    #[pyo3(signature = (from_bus, to_bus, r_ohm_per_km, x_ohm_per_km, b_us_per_km, length_km, base_kv, rate_a_mva=0.0, circuit=1))]
    pub fn add_line(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        r_ohm_per_km: f64,
        x_ohm_per_km: f64,
        b_us_per_km: f64,
        length_km: f64,
        base_kv: f64,
        rate_a_mva: f64,
        circuit: i64,
    ) -> PyResult<()> {
        if length_km <= 0.0 {
            return Err(PyValueError::new_err("length_km must be > 0"));
        }
        if base_kv <= 0.0 {
            return Err(PyValueError::new_err("base_kv must be > 0"));
        }
        let net = Arc::make_mut(&mut self.inner);
        if !net.buses.iter().any(|bus| bus.number == from_bus) {
            return Err(NetworkError::new_err(format!(
                "from_bus {from_bus} not found in network"
            )));
        }
        if !net.buses.iter().any(|bus| bus.number == to_bus) {
            return Err(NetworkError::new_err(format!(
                "to_bus {to_bus} not found in network"
            )));
        }

        let z_base = base_kv * base_kv / net.base_mva;
        let r_pu = r_ohm_per_km * length_km / z_base;
        let x_pu = x_ohm_per_km * length_km / z_base;
        // b_us_per_km is in µS/km; convert to S then to p.u.:
        // b_pu = b_us_per_km * 1e-6 * length_km * z_base
        let b_pu = b_us_per_km * 1e-6 * length_km * z_base;

        let mut br = Branch::new_line(from_bus, to_bus, r_pu, x_pu, b_pu);
        br.rating_a_mva = rate_a_mva;
        br.circuit = circuit.to_string();
        net.branches.push(br);
        Ok(())
    }

    /// Add a transformer from nameplate (engineering) parameters.
    ///
    /// Converts percent impedance on the transformer's own MVA base to per-unit
    /// on the system base (``base_mva``, typically 100 MVA).
    ///
    /// Args:
    ///   from_bus:     HV (primary) bus number.
    ///   to_bus:       LV (secondary) bus number.
    ///   mva_rating:   Transformer MVA rating.
    ///   v1_kv:        Primary (from_bus) rated voltage (kV).
    ///   v2_kv:        Secondary (to_bus) rated voltage (kV).
    ///   z_percent:    Impedance in percent on transformer MVA base (e.g. 8.0 for 8%).
    ///   r_percent:    Resistance in percent on transformer MVA base (default 0.5).
    ///   tap_pu:       Off-nominal tap ratio in p.u. (default 1.0).
    ///   shift_deg:    Phase shift angle in degrees (default 0.0).
    ///   rate_a_mva:   Thermal rating in MVA. 0 = use mva_rating.
    ///   circuit:      Circuit identifier (default 1).
    ///
    /// Raises:
    ///   ValueError: if from_bus or to_bus is not in the network, if
    ///     mva_rating <= 0, or if r_percent > z_percent.
    #[pyo3(signature = (from_bus, to_bus, mva_rating, v1_kv, v2_kv, z_percent, r_percent=0.5, tap_pu=1.0, shift_deg=0.0, rate_a_mva=0.0, circuit=1))]
    #[allow(clippy::too_many_arguments, unused_variables)]
    pub fn add_transformer(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        mva_rating: f64,
        v1_kv: f64,
        v2_kv: f64,
        z_percent: f64,
        r_percent: f64,
        tap_pu: f64,
        shift_deg: f64,
        rate_a_mva: f64,
        circuit: i64,
    ) -> PyResult<()> {
        if mva_rating <= 0.0 {
            return Err(PyValueError::new_err("mva_rating must be > 0"));
        }
        if r_percent > z_percent {
            return Err(PyValueError::new_err("r_percent cannot exceed z_percent"));
        }
        let net = Arc::make_mut(&mut self.inner);
        if !net.buses.iter().any(|bus| bus.number == from_bus) {
            return Err(NetworkError::new_err(format!(
                "from_bus {from_bus} not found in network"
            )));
        }
        if !net.buses.iter().any(|bus| bus.number == to_bus) {
            return Err(NetworkError::new_err(format!(
                "to_bus {to_bus} not found in network"
            )));
        }

        // Impedance on transformer's own MVA base
        let x_pct = (z_percent * z_percent - r_percent * r_percent).sqrt();
        let r_xfmr_pu = r_percent / 100.0;
        let x_xfmr_pu = x_pct / 100.0;

        // Convert to system base
        let base_mva = net.base_mva;
        let r_pu = r_xfmr_pu * (base_mva / mva_rating);
        let x_pu = x_xfmr_pu * (base_mva / mva_rating);

        let effective_rate = if rate_a_mva == 0.0 {
            mva_rating
        } else {
            rate_a_mva
        };

        let mut br = Branch::new_line(from_bus, to_bus, r_pu, x_pu, 0.0);
        br.rating_a_mva = effective_rate;
        br.circuit = circuit.to_string();
        br.tap = tap_pu;
        br.phase_shift_rad = shift_deg.to_radians();
        net.branches.push(br);
        Ok(())
    }

    /// Set harmonic analysis parameters on an existing branch.
    ///
    /// Args:
    ///   from_bus:           From-bus number.
    ///   to_bus:             To-bus number.
    ///   skin_effect_alpha:  IEC 60287 skin-effect coefficient.
    ///                       If > 0: ``R_h = R × (1 + alpha × (h − 1))``.
    ///                       If 0: ``R_h = R × sqrt(h)`` (default).
    ///   delta_connected:    If ``True``, block triplen harmonics (3rd, 6th, ...).
    ///   circuit:            Circuit identifier (default 1).
    ///
    /// Raises:
    ///   ValueError: if no matching branch is found.
    #[pyo3(signature = (from_bus, to_bus, skin_effect_alpha=None, delta_connected=None, circuit=1))]
    pub fn set_branch_harmonic_params(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        skin_effect_alpha: Option<f64>,
        delta_connected: Option<bool>,
        circuit: i64,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let c = circuit.to_string();
        let idx = find_branch_str(&net.branches, from_bus, to_bus, &c).ok_or_else(|| {
            NetworkError::new_err(format!("Branch {from_bus}-{to_bus} circuit {c} not found"))
        })?;
        if let Some(alpha) = skin_effect_alpha {
            net.branches[idx]
                .harmonic
                .get_or_insert_with(HarmonicData::default)
                .skin_effect_alpha = alpha;
        }
        if let Some(delta) = delta_connected {
            net.branches[idx]
                .zero_seq
                .get_or_insert_with(ZeroSeqData::default)
                .delta_connected = delta;
        }
        Ok(())
    }

    /// Set the long-term thermal rating of a branch (MVA).
    ///
    /// Raises:
    ///   ValueError: if no matching branch is found.
    #[pyo3(signature = (from_bus, to_bus, rate_a_mva, circuit=1))]
    pub fn set_branch_rating(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        rate_a_mva: f64,
        circuit: i64,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let c = circuit.to_string();
        let idx = find_branch_str(&net.branches, from_bus, to_bus, &c).ok_or_else(|| {
            NetworkError::new_err(format!("Branch {from_bus}-{to_bus} circuit {c} not found"))
        })?;
        net.branches[idx].rating_a_mva = rate_a_mva;
        Ok(())
    }

    /// Set zero-sequence impedance for a branch.
    ///
    /// Args:
    ///   from_bus:  From-bus number.
    ///   to_bus:    To-bus number.
    ///   circuit:   Circuit identifier.
    ///   r0:        Zero-sequence resistance (p.u.).
    ///   x0:        Zero-sequence reactance (p.u.).
    ///   b0:        Zero-sequence charging susceptance (p.u., default 0.0).
    ///
    /// Returns:
    ///   True if the branch was found and updated, False otherwise.
    #[pyo3(signature = (from_bus, to_bus, circuit, r0, x0, b0=0.0))]
    fn set_branch_sequence(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        circuit: &str,
        r0: f64,
        x0: f64,
        b0: f64,
    ) -> bool {
        let net = Arc::make_mut(&mut self.inner);
        net.set_branch_sequence(from_bus, to_bus, circuit, r0, x0, b0)
    }

    /// Get zero-sequence impedance for a branch.
    ///
    /// Args:
    ///   from_bus:  From-bus number.
    ///   to_bus:    To-bus number.
    ///   circuit:   Circuit identifier.
    ///
    /// Returns:
    ///   ``(r0, x0, b0)`` if the branch exists and has zero-sequence data,
    ///   ``None`` otherwise.
    fn get_branch_sequence(
        &self,
        from_bus: u32,
        to_bus: u32,
        circuit: &str,
    ) -> Option<(f64, f64, f64)> {
        self.inner.get_branch_sequence(from_bus, to_bus, circuit)
    }

    /// Set the impedance parameters of a branch.
    ///
    /// Only the supplied parameters are updated; ``None`` values leave the
    /// existing value unchanged.
    ///
    /// Args:
    ///   from_bus:  From-bus number.
    ///   to_bus:    To-bus number.
    ///   circuit:   Circuit identifier (default 1).
    ///   r_pu:      Series resistance (p.u.).
    ///   x_pu:      Series reactance (p.u.).
    ///   b_pu:      Total line charging susceptance (p.u.).
    ///
    /// Returns:
    ///   True if the branch was found and updated.
    ///
    /// Raises:
    ///   ValueError: if no matching branch is found.
    #[pyo3(signature = (from_bus, to_bus, circuit=1, r_pu=None, x_pu=None, b_pu=None))]
    pub fn set_branch_impedance(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        circuit: i64,
        r_pu: Option<f64>,
        x_pu: Option<f64>,
        b_pu: Option<f64>,
    ) -> PyResult<bool> {
        let net = Arc::make_mut(&mut self.inner);
        let c = circuit.to_string();
        let idx = find_branch_str(&net.branches, from_bus, to_bus, &c).ok_or_else(|| {
            NetworkError::new_err(format!("Branch {from_bus}-{to_bus} circuit {c} not found"))
        })?;
        if let Some(r) = r_pu {
            net.branches[idx].r = r;
        }
        if let Some(x) = x_pu {
            net.branches[idx].x = x;
        }
        if let Some(b) = b_pu {
            net.branches[idx].b = b;
        }
        Ok(true)
    }

    /// Add a branch from an editable ``Branch`` object.
    pub fn add_branch_object(&mut self, branch: PyRef<'_, rich_objects::Branch>) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        if !net.buses.iter().any(|bus| bus.number == branch.from_bus) {
            return Err(NetworkError::new_err(format!(
                "from_bus {} not found in network",
                branch.from_bus
            )));
        }
        if !net.buses.iter().any(|bus| bus.number == branch.to_bus) {
            return Err(NetworkError::new_err(format!(
                "to_bus {} not found in network",
                branch.to_bus
            )));
        }
        let mut new_branch = Branch::new_line(branch.from_bus, branch.to_bus, 0.0, 0.0, 0.0);
        new_branch.circuit = branch.circuit.clone();
        net.branches.push(new_branch);
        self.update_branch_object(branch)
    }

    /// Apply the fields of an editable ``Branch`` object back onto the network.
    pub fn update_branch_object(
        &mut self,
        branch: PyRef<'_, rich_objects::Branch>,
    ) -> PyResult<()> {
        let transformer_connection = parse_transformer_connection(&branch.transformer_connection)?;
        let tap_mode = parse_tap_mode(&branch.tap_mode)?;
        let phase_mode = parse_phase_mode(&branch.phase_mode)?;
        let net = Arc::make_mut(&mut self.inner);
        let target = net
            .branches
            .iter_mut()
            .find(|existing| {
                existing.from_bus == branch.from_bus
                    && existing.to_bus == branch.to_bus
                    && existing.circuit == branch.circuit
            })
            .ok_or_else(|| {
                NetworkError::new_err(format!(
                    "Branch {}-{} circuit {} not found",
                    branch.from_bus, branch.to_bus, branch.circuit
                ))
            })?;
        target.r = branch.r_pu;
        target.x = branch.x_pu;
        target.b = branch.b_pu;
        target.rating_a_mva = branch.rate_a_mva;
        target.rating_b_mva = branch.rate_b_mva;
        target.rating_c_mva = branch.rate_c_mva;
        target.tap = branch.tap;
        target.phase_shift_rad = branch.shift_deg.to_radians();
        target.in_service = branch.in_service;
        target.angle_diff_min_rad = branch.angmin_deg.map(f64::to_radians);
        target.angle_diff_max_rad = branch.angmax_deg.map(f64::to_radians);
        target.g_pi = branch.g_pi;
        target.g_mag = branch.g_mag;
        target.b_mag = branch.b_mag;
        target
            .transformer_data
            .get_or_insert_with(TransformerData::default)
            .transformer_connection = transformer_connection;
        target
            .transformer_data
            .get_or_insert_with(TransformerData::default)
            .oil_temp_limit_c = branch.oil_temp_limit_c;
        target
            .transformer_data
            .get_or_insert_with(TransformerData::default)
            .winding_temp_limit_c = branch.winding_temp_limit_c;
        target
            .transformer_data
            .get_or_insert_with(TransformerData::default)
            .impedance_limit_ohm = branch.impedance_limit_ohm;
        target
            .zero_seq
            .get_or_insert_with(ZeroSeqData::default)
            .delta_connected = branch.delta_connected;
        target
            .harmonic
            .get_or_insert_with(HarmonicData::default)
            .skin_effect_alpha = branch.skin_effect_alpha;
        target
            .opf_control
            .get_or_insert_with(BranchOpfControl::default)
            .tap_mode = tap_mode;
        target
            .opf_control
            .get_or_insert_with(BranchOpfControl::default)
            .tap_min = branch.tap_min;
        target
            .opf_control
            .get_or_insert_with(BranchOpfControl::default)
            .tap_max = branch.tap_max;
        target
            .opf_control
            .get_or_insert_with(BranchOpfControl::default)
            .phase_mode = phase_mode;
        target
            .opf_control
            .get_or_insert_with(BranchOpfControl::default)
            .phase_min_rad = branch.phase_min_deg.to_radians();
        target
            .opf_control
            .get_or_insert_with(BranchOpfControl::default)
            .phase_max_rad = branch.phase_max_deg.to_radians();
        if !branch.has_saturation {
            if let Some(h) = target.harmonic.as_mut() {
                h.saturation = None;
            }
        } else if target
            .harmonic
            .as_ref()
            .and_then(|h| h.saturation.as_ref())
            .is_none()
        {
            return Err(PyValueError::new_err(
                "branch.has_saturation cannot create a new saturation curve by itself; keep an existing curve or use a dedicated saturation API",
            ));
        }
        target
            .harmonic
            .get_or_insert_with(HarmonicData::default)
            .core_type = branch.core_type.clone().and_then(|name| name.parse().ok());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Generator operations
// ---------------------------------------------------------------------------

#[pymethods]
impl Network {
    /// Add a generator at a bus.
    ///
    /// Args:
    ///   bus:        Bus number.
    ///   p_mw:      Real power output (MW).
    ///   pmax_mw:    Maximum real power (MW).
    ///   pmin_mw:    Minimum real power (MW).
    ///   vs_pu:      Voltage setpoint (p.u.).
    ///   qmax_mvar:  Maximum reactive power (MVAr).
    ///   qmin_mvar:  Minimum reactive power (MVAr).
    ///   machine_id: Machine identifier string preserved as imported metadata.
    ///   id:         Canonical generator identifier. When omitted, Surge assigns one.
    ///
    /// Raises:
    ///   ValueError: if the bus is not found.
    #[pyo3(signature = (bus, p_mw, pmax_mw, pmin_mw=0.0, vs_pu=1.0, qmax_mvar=9999.0, qmin_mvar=-9999.0, machine_id="1", id=None))]
    pub fn add_generator(
        &mut self,
        bus: u32,
        p_mw: f64,
        pmax_mw: f64,
        pmin_mw: f64,
        vs_pu: f64,
        qmax_mvar: f64,
        qmin_mvar: f64,
        machine_id: &str,
        id: Option<String>,
    ) -> PyResult<String> {
        let net = Arc::make_mut(&mut self.inner);
        if !net.buses.iter().any(|b| b.number == bus) {
            return Err(NetworkError::new_err(format!("Bus {bus} not found")));
        }
        let mut generator = Generator::new(bus, p_mw, vs_pu);
        generator.pmax = pmax_mw;
        generator.pmin = pmin_mw;
        generator.qmax = qmax_mvar;
        generator.qmin = qmin_mvar;
        generator.machine_id = Some(machine_id.to_string());
        if let Some(id) = id {
            generator.id = id;
        }
        net.generators.push(generator);
        net.canonicalize_generator_ids();
        Ok(net
            .generators
            .last()
            .map(|generator| generator.id.clone())
            .expect("generator push should create a last element"))
    }

    /// Add a storage resource (BESS, pumped hydro in storage mode) as a
    /// generator with ``StorageParams`` attached.
    ///
    /// The resulting generator has ``pmin = -charge_mw_max`` (negative) and
    /// ``pmax = discharge_mw_max`` (positive), matching the convention the
    /// solver uses for bidirectional resources.
    ///
    /// Args:
    ///   bus:              Bus number where the storage is connected.
    ///   charge_mw_max:    Maximum charge rate in MW (positive; stored as
    ///                     negative pmin).
    ///   discharge_mw_max: Maximum discharge rate in MW (positive; stored
    ///                     as pmax).
    ///   params:           ``StorageParams`` describing SOC bounds,
    ///                     efficiency, dispatch mode, and offers.
    ///   machine_id:       PSS/E machine id (default ``"1"``).
    ///   id:               Explicit canonical id; auto-assigned
    ///                     ``gen_{bus}_{ordinal}`` when omitted.
    ///
    /// Returns:
    ///   Canonical generator id of the newly-added storage resource.
    ///
    /// Raises:
    ///   ValueError: if the bus number is not in the network, or the
    ///     ``StorageParams`` fail validation
    ///     (``efficiency ∉ (0, 1]``, invalid SOC range, etc.).
    #[pyo3(signature = (bus, charge_mw_max, discharge_mw_max, params, machine_id=None, id=None))]
    pub fn add_storage(
        &mut self,
        bus: u32,
        charge_mw_max: f64,
        discharge_mw_max: f64,
        params: PyRef<'_, rich_objects::StorageParams>,
        machine_id: Option<String>,
        id: Option<String>,
    ) -> PyResult<String> {
        if charge_mw_max < 0.0 {
            return Err(PyValueError::new_err(format!(
                "charge_mw_max must be >= 0, got {charge_mw_max}"
            )));
        }
        if discharge_mw_max < 0.0 {
            return Err(PyValueError::new_err(format!(
                "discharge_mw_max must be >= 0, got {discharge_mw_max}"
            )));
        }
        let storage_params = params.to_core()?;
        storage_params
            .validate()
            .map_err(|e| PyValueError::new_err(format!("invalid StorageParams: {e}")))?;

        let net = Arc::make_mut(&mut self.inner);
        if !net.buses.iter().any(|b| b.number == bus) {
            return Err(NetworkError::new_err(format!("Bus {bus} not found")));
        }

        let mut generator = Generator::new(bus, 0.0, 1.0);
        generator.pmax = discharge_mw_max;
        generator.pmin = -charge_mw_max;
        generator.qmax = 0.0;
        generator.qmin = 0.0;
        generator.machine_id = Some(machine_id.unwrap_or_else(|| "1".to_string()));
        if let Some(explicit_id) = id {
            generator.id = explicit_id;
        }
        generator.storage = Some(storage_params);

        net.generators.push(generator);
        net.canonicalize_generator_ids();
        Ok(net
            .generators
            .last()
            .map(|generator| generator.id.clone())
            .expect("generator push should create a last element"))
    }

    /// Attach (or replace) ``StorageParams`` on an existing generator.
    ///
    /// Raises:
    ///   ValueError: if no matching generator is found, or the
    ///     ``StorageParams`` fail validation.
    pub fn set_generator_storage(
        &mut self,
        id: &str,
        params: PyRef<'_, rich_objects::StorageParams>,
    ) -> PyResult<()> {
        let storage_params = params.to_core()?;
        storage_params
            .validate()
            .map_err(|e| PyValueError::new_err(format!("invalid StorageParams: {e}")))?;
        let net = Arc::make_mut(&mut self.inner);
        find_generator_mut_by_id(&mut net.generators, id)?.storage = Some(storage_params);
        Ok(())
    }

    /// Remove ``StorageParams`` from a generator, turning it back into a
    /// conventional one-directional unit.
    pub fn clear_generator_storage(&mut self, id: &str) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        find_generator_mut_by_id(&mut net.generators, id)?.storage = None;
        Ok(())
    }

    /// Remove a generator.
    ///
    /// Raises:
    ///   ValueError: if no matching generator is found.
    pub fn remove_generator(&mut self, id: &str) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let before = net.generators.len();
        net.generators.retain(|generator| generator.id != id);
        if net.generators.len() == before {
            return Err(NetworkError::new_err(format!(
                "Generator id='{id}' not found"
            )));
        }
        Ok(())
    }

    /// Set the active power output of a generator (MW).
    ///
    /// Raises:
    ///   ValueError: if no matching generator is found.
    pub fn set_generator_p(&mut self, id: &str, p_mw: f64) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        find_generator_mut_by_id(&mut net.generators, id)?.p = p_mw;
        Ok(())
    }

    /// Set the reactive power output of a generator (MVAr).
    ///
    /// Raises:
    ///   ValueError: if no matching generator is found.
    pub fn set_generator_q(&mut self, id: &str, q_mvar: f64) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        find_generator_mut_by_id(&mut net.generators, id)?.q = q_mvar;
        Ok(())
    }

    /// Set the in-service status of a generator.
    ///
    /// Raises:
    ///   ValueError: if no matching generator is found.
    pub fn set_generator_in_service(&mut self, id: &str, in_service: bool) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        find_generator_mut_by_id(&mut net.generators, id)?.in_service = in_service;
        Ok(())
    }

    /// Set the real power limits of a generator (MW).
    ///
    /// Raises:
    ///   ValueError: if no matching generator is found.
    pub fn set_generator_limits(&mut self, id: &str, pmax_mw: f64, pmin_mw: f64) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let generator = find_generator_mut_by_id(&mut net.generators, id)?;
        generator.pmax = pmax_mw;
        generator.pmin = pmin_mw;
        Ok(())
    }

    /// Set the voltage setpoint of a generator (p.u.).
    ///
    /// Raises:
    ///   ValueError: if no matching generator is found.
    pub fn set_generator_setpoint(&mut self, id: &str, vs_pu: f64) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        find_generator_mut_by_id(&mut net.generators, id)?.voltage_setpoint_pu = vs_pu;
        Ok(())
    }

    /// Set the cost curve for a generator.
    ///
    /// For polynomial costs: ``coeffs`` = ``[c2, c1, c0]`` where
    /// ``cost = c2 * P^2 + c1 * P + c0`` (highest-order first, $/hr).
    ///
    /// Args:
    ///   id:         Canonical generator identifier.
    ///   coeffs:     Polynomial coefficients (highest-order first).
    ///
    /// Raises:
    ///   ValueError: if no matching generator is found.
    pub fn set_generator_cost(&mut self, id: &str, coeffs: Vec<f64>) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let generator = find_generator_mut_by_id(&mut net.generators, id)?;
        generator.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs,
        });
        Ok(())
    }

    /// Replace a generator's reserve offers.
    ///
    /// Each offer is ``(product_id, capacity_mw, cost_per_mwh)``.
    pub fn set_generator_reserve_offers(
        &mut self,
        id: &str,
        reserve_offers: Vec<(String, f64, f64)>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let generator = find_generator_mut_by_id(&mut net.generators, id)?;
        generator
            .market
            .get_or_insert_with(MarketParams::default)
            .reserve_offers = reserve_offers
            .into_iter()
            .map(
                |(product_id, capacity_mw, cost_per_mwh)| surge_network::market::ReserveOffer {
                    product_id,
                    capacity_mw,
                    cost_per_mwh,
                },
            )
            .collect();
        Ok(())
    }

    /// Replace a generator's reserve qualification flags.
    pub fn set_generator_qualifications(
        &mut self,
        id: &str,
        qualifications: HashMap<String, bool>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let generator = find_generator_mut_by_id(&mut net.generators, id)?;
        generator
            .market
            .get_or_insert_with(MarketParams::default)
            .qualifications = qualifications;
        Ok(())
    }

    /// Add a generator from an editable ``Generator`` object.
    pub fn add_generator_object(
        &mut self,
        generator: PyRef<'_, rich_objects::Generator>,
    ) -> PyResult<String> {
        let id = self.add_generator(
            generator.bus,
            generator.p_mw,
            generator.pmax_mw,
            generator.pmin_mw,
            generator.vs_pu,
            generator.qmax_mvar,
            generator.qmin_mvar,
            &generator.machine_id,
            if generator.id.is_empty() {
                None
            } else {
                Some(generator.id.clone())
            },
        )?;
        let commitment_status = parse_commitment_status(&generator.commitment_status)?;
        let cost = build_generator_cost_curve(&generator)?;
        let net = Arc::make_mut(&mut self.inner);
        let target = find_generator_mut_by_id(&mut net.generators, &id)?;
        target.p = generator.p_mw;
        target.q = generator.q_mvar;
        target.pmax = generator.pmax_mw;
        target.pmin = generator.pmin_mw;
        target.qmax = generator.qmax_mvar;
        target.qmin = generator.qmin_mvar;
        target.voltage_setpoint_pu = generator.vs_pu;
        target.machine_base_mva = generator.mbase_mva;
        target.in_service = generator.in_service;
        // Fuel params
        {
            let fuel = target.fuel.get_or_insert_with(FuelParams::default);
            fuel.fuel_type = generator.fuel_type.clone();
            fuel.heat_rate_btu_mwh = generator.heat_rate_btu_mwh;
            fuel.emission_rates.co2 = generator.co2_rate_t_per_mwh;
            fuel.emission_rates.nox = generator.nox_rate_t_per_mwh;
            fuel.emission_rates.so2 = generator.so2_rate_t_per_mwh;
            fuel.emission_rates.pm25 = generator.pm25_rate_t_per_mwh;
        }
        target.forced_outage_rate = generator.forced_outage_rate;
        // Ramping params
        {
            let ramping = target.ramping.get_or_insert_with(RampingParams::default);
            ramping.ramp_up_curve = generator.ramp_up_curve.clone();
            ramping.ramp_down_curve = generator.ramp_down_curve.clone();
            ramping.reg_ramp_up_curve = generator.reg_ramp_up_curve.clone();
        }
        // Commitment params
        {
            let commit = target
                .commitment
                .get_or_insert_with(CommitmentParams::default);
            commit.status = commitment_status;
            commit.min_up_time_hr = generator.min_up_time_hr;
            commit.min_down_time_hr = generator.min_down_time_hr;
            commit.max_up_time_hr = generator.max_up_time_hr;
            commit.min_run_at_pmin_hr = generator.min_run_at_pmin_hr;
            commit.max_starts_per_day = generator.max_starts_per_day;
            commit.max_starts_per_week = generator.max_starts_per_week;
            commit.max_energy_mwh_per_day = generator.max_energy_mwh_per_day;
            commit.startup_ramp_mw_per_min = generator.startup_ramp_mw_per_min;
            commit.shutdown_ramp_mw_per_min = generator.shutdown_ramp_mw_per_min;
        }
        target.quick_start = generator.quick_start;
        target.storage = generator
            .storage
            .as_ref()
            .map(rich_objects::StorageParams::to_core)
            .transpose()?;
        // Market params
        {
            let market = target.market.get_or_insert_with(MarketParams::default);
            market.reserve_offers = generator
                .reserve_offers
                .iter()
                .map(|(product_id, capacity_mw, cost_per_mwh)| {
                    surge_network::market::ReserveOffer {
                        product_id: product_id.clone(),
                        capacity_mw: *capacity_mw,
                        cost_per_mwh: *cost_per_mwh,
                    }
                })
                .collect();
            market.qualifications = generator.qualifications.clone();
        }
        // Reactive capability
        {
            let rc = target
                .reactive_capability
                .get_or_insert_with(ReactiveCapability::default);
            rc.pc1 = generator.pc1_mw;
            rc.pc2 = generator.pc2_mw;
            rc.qc1min = generator.qc1min_mvar;
            rc.qc1max = generator.qc1max_mvar;
            rc.qc2min = generator.qc2min_mvar;
            rc.qc2max = generator.qc2max_mvar;
            rc.pq_curve = generator.pq_curve.clone();
        }
        target.h_inertia_s = generator.h_inertia_s;
        // Fault data
        {
            let fd = target.fault_data.get_or_insert_with(GenFaultData::default);
            fd.xs = generator.xs_pu;
            fd.x2_pu = generator.x2_pu;
            fd.zn = generator.zn_pu.map(|(re, im)| Complex64::new(re, im));
        }
        target.agc_participation_factor = generator.apf;
        target.cost = cost;
        Ok(id)
    }

    /// Apply the fields of an editable ``Generator`` object back onto the network.
    pub fn update_generator_object(
        &mut self,
        generator: PyRef<'_, rich_objects::Generator>,
    ) -> PyResult<()> {
        let commitment_status = parse_commitment_status(&generator.commitment_status)?;
        let cost = build_generator_cost_curve(&generator)?;
        let net = Arc::make_mut(&mut self.inner);
        let target = find_generator_mut_by_id(&mut net.generators, &generator.id)?;
        target.p = generator.p_mw;
        target.q = generator.q_mvar;
        target.pmax = generator.pmax_mw;
        target.pmin = generator.pmin_mw;
        target.qmax = generator.qmax_mvar;
        target.qmin = generator.qmin_mvar;
        target.voltage_setpoint_pu = generator.vs_pu;
        target.machine_base_mva = generator.mbase_mva;
        target.in_service = generator.in_service;
        // Fuel params
        {
            let fuel = target.fuel.get_or_insert_with(FuelParams::default);
            fuel.fuel_type = generator.fuel_type.clone();
            fuel.heat_rate_btu_mwh = generator.heat_rate_btu_mwh;
            fuel.emission_rates.co2 = generator.co2_rate_t_per_mwh;
            fuel.emission_rates.nox = generator.nox_rate_t_per_mwh;
            fuel.emission_rates.so2 = generator.so2_rate_t_per_mwh;
            fuel.emission_rates.pm25 = generator.pm25_rate_t_per_mwh;
        }
        target.forced_outage_rate = generator.forced_outage_rate;
        // Ramping params
        {
            let ramping = target.ramping.get_or_insert_with(RampingParams::default);
            ramping.ramp_up_curve = generator.ramp_up_curve.clone();
            ramping.ramp_down_curve = generator.ramp_down_curve.clone();
            ramping.reg_ramp_up_curve = generator.reg_ramp_up_curve.clone();
        }
        // Commitment params
        {
            let commit = target
                .commitment
                .get_or_insert_with(CommitmentParams::default);
            commit.status = commitment_status;
            commit.min_up_time_hr = generator.min_up_time_hr;
            commit.min_down_time_hr = generator.min_down_time_hr;
            commit.max_up_time_hr = generator.max_up_time_hr;
            commit.min_run_at_pmin_hr = generator.min_run_at_pmin_hr;
            commit.max_starts_per_day = generator.max_starts_per_day;
            commit.max_starts_per_week = generator.max_starts_per_week;
            commit.max_energy_mwh_per_day = generator.max_energy_mwh_per_day;
            commit.startup_ramp_mw_per_min = generator.startup_ramp_mw_per_min;
            commit.shutdown_ramp_mw_per_min = generator.shutdown_ramp_mw_per_min;
        }
        target.quick_start = generator.quick_start;
        target.storage = generator
            .storage
            .as_ref()
            .map(rich_objects::StorageParams::to_core)
            .transpose()?;
        // Market params
        {
            let market = target.market.get_or_insert_with(MarketParams::default);
            market.reserve_offers = generator
                .reserve_offers
                .iter()
                .map(|(product_id, capacity_mw, cost_per_mwh)| {
                    surge_network::market::ReserveOffer {
                        product_id: product_id.clone(),
                        capacity_mw: *capacity_mw,
                        cost_per_mwh: *cost_per_mwh,
                    }
                })
                .collect();
            market.qualifications = generator.qualifications.clone();
        }
        // Reactive capability
        {
            let rc = target
                .reactive_capability
                .get_or_insert_with(ReactiveCapability::default);
            rc.pc1 = generator.pc1_mw;
            rc.pc2 = generator.pc2_mw;
            rc.qc1min = generator.qc1min_mvar;
            rc.qc1max = generator.qc1max_mvar;
            rc.qc2min = generator.qc2min_mvar;
            rc.qc2max = generator.qc2max_mvar;
            rc.pq_curve = generator.pq_curve.clone();
        }
        target.h_inertia_s = generator.h_inertia_s;
        // Fault data
        {
            let fd = target.fault_data.get_or_insert_with(GenFaultData::default);
            fd.xs = generator.xs_pu;
            fd.x2_pu = generator.x2_pu;
            fd.zn = generator.zn_pu.map(|(re, im)| Complex64::new(re, im));
        }
        target.agc_participation_factor = generator.apf;
        target.cost = cost;

        // Write back startup_cost_tiers into the market.energy_offer structure.
        if !generator.startup_cost_tiers.is_empty() {
            let tiers: Vec<surge_network::market::StartupTier> = generator
                .startup_cost_tiers
                .iter()
                .map(|&(max_offline_hours, cost, sync_time_min)| {
                    surge_network::market::StartupTier {
                        max_offline_hours,
                        cost,
                        sync_time_min,
                    }
                })
                .collect();
            let market = target.market.get_or_insert_with(MarketParams::default);
            match &mut market.energy_offer {
                Some(eo) => eo.submitted.startup_tiers = tiers,
                None => {
                    market.energy_offer = Some(surge_network::market::EnergyOffer {
                        submitted: surge_network::market::OfferCurve {
                            segments: Vec::new(),
                            no_load_cost: 0.0,
                            startup_tiers: tiers,
                        },
                        mitigated: None,
                        mitigation_active: false,
                    });
                }
            }
        } else if let Some(market) = &mut target.market {
            if let Some(eo) = &mut market.energy_offer {
                eo.submitted.startup_tiers.clear();
            }
        }

        Ok(())
    }
}

#[pymethods]
impl Network {
    /// Enable or disable AC voltage regulation for a generator.
    ///
    /// Raises:
    ///   ValueError: if no matching generator is found.
    #[pyo3(signature = (id, voltage_regulated))]
    pub fn set_generator_voltage_regulated(
        &mut self,
        id: &str,
        voltage_regulated: bool,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        find_generator_mut_by_id(&mut net.generators, id)?.voltage_regulated = voltage_regulated;
        Ok(())
    }

    /// Set the regulated bus for a generator. ``None`` restores local regulation.
    ///
    /// Raises:
    ///   ValueError: if no matching generator is found.
    #[pyo3(signature = (id, regulated_bus=None))]
    pub fn set_generator_regulated_bus(
        &mut self,
        id: &str,
        regulated_bus: Option<u32>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        find_generator_mut_by_id(&mut net.generators, id)?.reg_bus = regulated_bus;
        Ok(())
    }

    /// Attach a GO Competition Challenge 3 §4.6 linear p-q linking
    /// constraint to a generator. ``kind`` is one of ``"equality"``
    /// (eq 116, J^pqe), ``"upper"`` (eq 114, J^pqmax), or ``"lower"``
    /// (eq 115, J^pqmin). The intercept and slope are in per-unit on
    /// the device base.
    ///
    /// Raises:
    ///   ValueError: if no matching generator is found or ``kind`` is
    ///   invalid.
    #[pyo3(signature = (id, kind, q_at_p_zero_pu, beta))]
    pub fn set_generator_pq_linear_link(
        &mut self,
        id: &str,
        kind: &str,
        q_at_p_zero_pu: f64,
        beta: f64,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let generator = find_generator_mut_by_id(&mut net.generators, id)?;
        let rc = generator
            .reactive_capability
            .get_or_insert_with(surge_network::network::ReactiveCapability::default);
        let link = surge_network::network::PqLinearLink {
            q_at_p_zero_pu,
            beta,
        };
        match kind {
            "equality" => rc.pq_linear_equality = Some(link),
            "upper" => rc.pq_linear_upper = Some(link),
            "lower" => rc.pq_linear_lower = Some(link),
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "set_generator_pq_linear_link: unknown kind {other:?} (expected \"equality\", \"upper\", or \"lower\")"
                )));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Scale + utility
// ---------------------------------------------------------------------------

#[pymethods]
impl Network {
    /// Scale all Load objects by ``factor``.
    ///
    /// Args:
    ///   factor: Multiplicative scale factor (e.g. 0.5 to halve all load).
    ///   area:   If given, only scale loads whose bus is in this area number.
    #[pyo3(signature = (factor, area=None))]
    pub fn scale_loads(&mut self, factor: f64, area: Option<u32>) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        // Build bus→area map for Load object filtering.
        let area_map: HashMap<u32, u32> = net.buses.iter().map(|b| (b.number, b.area)).collect();
        for load in net.loads.iter_mut() {
            if area.is_none() || area_map.get(&load.bus).copied() == area {
                load.active_power_demand_mw *= factor;
                load.reactive_power_demand_mvar *= factor;
            }
        }
        Ok(())
    }

    /// Scale all in-service generator dispatch by ``factor``.
    ///
    /// Args:
    ///   factor: Multiplicative scale factor.
    ///   area:   If given, only scale generators whose bus is in this area number.
    #[pyo3(signature = (factor, area=None))]
    pub fn scale_generators(&mut self, factor: f64, area: Option<u32>) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let area_map: HashMap<u32, u32> = net.buses.iter().map(|b| (b.number, b.area)).collect();
        for generator in net.generators.iter_mut() {
            if !generator.in_service {
                continue;
            }
            if area.is_none() || area_map.get(&generator.bus).copied() == area {
                generator.p *= factor;
            }
        }
        Ok(())
    }

    /// Return an independent deep copy of this network.
    ///
    /// Mutations to the copy do not affect the original, and vice versa.
    pub fn copy(&self) -> Network {
        Network {
            inner: Arc::new((*self.inner).clone()),
            oltc_controls: self.oltc_controls.clone(),
            switched_shunts: self.switched_shunts.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Interface and Flowgate operations
// ---------------------------------------------------------------------------

#[pymethods]
impl Network {
    /// Add a transmission interface (a set of branches defining a flow boundary).
    ///
    /// Args:
    ///   name:             Interface name (e.g. ``"Houston Import"``).
    ///   members:          List of ``((from_bus, to_bus, circuit), coefficient)``
    ///                     entries describing the weighted boundary.
    ///   limit_forward_mw: MW limit in the forward direction.
    ///   limit_reverse_mw: MW limit in the reverse direction (positive magnitude).
    ///
    /// Raises:
    ///   ValueError: if a referenced branch does not exist in the network.
    #[pyo3(signature = (name, members, limit_forward_mw, limit_reverse_mw=0.0))]
    pub fn add_interface(
        &mut self,
        name: String,
        members: Vec<(PyBranchKey, f64)>,
        limit_forward_mw: f64,
        limit_reverse_mw: f64,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        for (branch, _) in &members {
            let (fb, tb, ckt): (u32, u32, String) = branch.clone().into();
            if find_branch_str(&net.branches, fb, tb, &ckt).is_none() {
                return Err(NetworkError::new_err(format!(
                    "Branch {fb}-{tb} circuit {ckt} not found in network"
                )));
            }
        }
        net.interfaces.push(surge_network::network::Interface {
            name,
            members: members
                .into_iter()
                .map(|(branch, coefficient)| WeightedBranchRef {
                    branch: BranchRef::from(<(u32, u32, String)>::from(branch)),
                    coefficient,
                })
                .collect(),
            limit_forward_mw,
            limit_reverse_mw,
            in_service: true,
            limit_forward_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
        });
        Ok(())
    }

    /// Remove a transmission interface by name.
    ///
    /// Raises:
    ///   ValueError: if no interface with the given name exists.
    pub fn remove_interface(&mut self, name: &str) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let before = net.interfaces.len();
        net.interfaces.retain(|iface| iface.name != name);
        if net.interfaces.len() == before {
            return Err(NetworkError::new_err(format!(
                "Interface '{name}' not found"
            )));
        }
        Ok(())
    }

    /// Add a flowgate (a monitored element under a specific contingency).
    ///
    /// Args:
    ///   name:               Flowgate name (e.g. ``"FG_123"``).
    ///   monitored:          List of ``((from_bus, to_bus, circuit), coefficient)``
    ///                       entries for the monitored elements.
    ///   contingency_branch: ``(from_bus, to_bus, circuit)`` of the contingency
    ///                       element, or ``None`` for a base-case-only flowgate.
    ///   limit_mw:           MW limit.
    ///
    /// Raises:
    ///   ValueError: if a referenced branch does not exist in the network.
    #[pyo3(signature = (name, monitored, limit_mw, contingency_branch=None))]
    pub fn add_flowgate(
        &mut self,
        name: String,
        monitored: Vec<(PyBranchKey, f64)>,
        limit_mw: f64,
        contingency_branch: Option<PyBranchKey>,
    ) -> PyResult<()> {
        let contingency_branch = contingency_branch.map(Into::into);
        let net = Arc::make_mut(&mut self.inner);
        for (branch, _) in &monitored {
            let (fb, tb, ckt): (u32, u32, String) = branch.clone().into();
            if find_branch_str(&net.branches, fb, tb, &ckt).is_none() {
                return Err(NetworkError::new_err(format!(
                    "Monitored branch {fb}-{tb} circuit {ckt} not found in network"
                )));
            }
        }
        if let Some((fb, tb, ref ckt)) = contingency_branch
            && find_branch_str(&net.branches, fb, tb, ckt).is_none()
        {
            return Err(NetworkError::new_err(format!(
                "Contingency branch {fb}-{tb} circuit {ckt} not found in network"
            )));
        }
        net.flowgates.push(surge_network::network::Flowgate {
            name,
            monitored: monitored
                .into_iter()
                .map(|(branch, coefficient)| WeightedBranchRef {
                    branch: BranchRef::from(<(u32, u32, String)>::from(branch)),
                    coefficient,
                })
                .collect(),
            contingency_branch: contingency_branch.map(BranchRef::from),
            limit_mw,
            limit_reverse_mw: 0.0,
            in_service: true,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: Vec::new(),
            hvdc_band_coefficients: Vec::new(),
            limit_mw_active_period: None,
            breach_sides: surge_network::network::FlowgateBreachSides::Both,
        });
        Ok(())
    }

    /// Remove a flowgate by name.
    ///
    /// Raises:
    ///   ValueError: if no flowgate with the given name exists.
    pub fn remove_flowgate(&mut self, name: &str) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let before = net.flowgates.len();
        net.flowgates.retain(|fg| fg.name != name);
        if net.flowgates.len() == before {
            return Err(NetworkError::new_err(format!(
                "Flowgate '{name}' not found"
            )));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Load operations (#13)
    // -----------------------------------------------------------------------

    /// Add a load to the network.
    ///
    /// Unlike ``set_bus_load`` which edits the bus-level aggregate, this method
    /// appends a discrete ``Load`` record to ``network.loads``.  Multiple loads
    /// may share the same bus and are distinguished by ``load_id``.
    ///
    /// Args:
    ///   bus:        Bus number.
    ///   pd_mw:      Real power demand (MW).
    ///   qd_mvar:    Reactive power demand (MVAr).
    ///   load_id:    Load identifier string (default ``"1"``).
    ///   conforming: Whether the load scales with the area forecast (default ``True``).
    ///
    /// Raises:
    ///   NetworkError: if the bus does not exist.
    #[pyo3(signature = (bus, pd_mw, qd_mvar, load_id="1", conforming=true))]
    pub fn add_load(
        &mut self,
        bus: u32,
        pd_mw: f64,
        qd_mvar: f64,
        load_id: &str,
        conforming: bool,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let bm = net.bus_index_map();
        if !bm.contains_key(&bus) {
            return Err(NetworkError::new_err(format!("Bus {bus} not found")));
        }
        net.loads.push(Load {
            bus,
            active_power_demand_mw: pd_mw,
            reactive_power_demand_mvar: qd_mvar,
            in_service: true,
            conforming,
            id: load_id.to_string(),
            ..Default::default()
        });
        Ok(())
    }

    /// Remove a load from the network.
    ///
    /// Removes the first load matching ``(bus, load_id)``.
    ///
    /// Args:
    ///   bus:     Bus number.
    ///   load_id: Load identifier string (default ``"1"``).
    ///
    /// Raises:
    ///   NetworkError: if no matching load is found.
    #[pyo3(signature = (bus, load_id="1"))]
    pub fn remove_load(&mut self, bus: u32, load_id: &str) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let idx = net
            .loads
            .iter()
            .position(|l| l.bus == bus && l.id == load_id)
            .ok_or_else(|| {
                NetworkError::new_err(format!("Load at bus {bus} with id '{load_id}' not found"))
            })?;
        net.loads.remove(idx);
        Ok(())
    }

    /// Set a load's in-service status.
    ///
    /// Args:
    ///   bus:        Bus number.
    ///   in_service: ``True`` to energise, ``False`` to de-energise.
    ///   load_id:    Load identifier string (default ``"1"``).
    ///
    /// Raises:
    ///   NetworkError: if no matching load is found.
    #[pyo3(signature = (bus, in_service, load_id="1"))]
    pub fn set_load_in_service(
        &mut self,
        bus: u32,
        in_service: bool,
        load_id: &str,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let load = net
            .loads
            .iter_mut()
            .find(|l| l.bus == bus && l.id == load_id)
            .ok_or_else(|| {
                NetworkError::new_err(format!("Load at bus {bus} with id '{load_id}' not found"))
            })?;
        load.in_service = in_service;
        Ok(())
    }

    /// Add a load from an editable ``Load`` object.
    pub fn add_load_object(&mut self, load: PyRef<'_, rich_objects::Load>) -> PyResult<()> {
        self.add_load(
            load.bus,
            load.pd_mw,
            load.qd_mvar,
            &load.id,
            load.conforming,
        )?;
        self.update_load_object(load)
    }

    /// Apply the fields of an editable ``Load`` object back onto the network.
    pub fn update_load_object(&mut self, load: PyRef<'_, rich_objects::Load>) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let target = net
            .loads
            .iter_mut()
            .find(|existing| existing.bus == load.bus && existing.id == load.id)
            .ok_or_else(|| {
                NetworkError::new_err(format!(
                    "Load at bus {} with id '{}' not found",
                    load.bus, load.id
                ))
            })?;
        target.active_power_demand_mw = load.pd_mw;
        target.reactive_power_demand_mvar = load.qd_mvar;
        target.in_service = load.in_service;
        target.conforming = load.conforming;
        Ok(())
    }

    /// Add a dispatchable-load resource to the network.
    ///
    /// The created resource is appended to ``network.dispatchable_loads`` and
    /// can participate in dispatch and reserve clearing.
    ///
    /// Supported archetypes: ``"Curtailable"`` and ``"Interruptible"``.
    #[pyo3(
        signature = (
            bus,
            p_sched_mw,
            q_sched_mvar=0.0,
            p_min_mw=0.0,
            cost_per_mwh=0.0,
            archetype="Curtailable",
            in_service=true,
            baseline_mw=None,
            reserve_offers=None,
            qualifications=None
        )
    )]
    pub fn add_dispatchable_load(
        &mut self,
        bus: u32,
        p_sched_mw: f64,
        q_sched_mvar: f64,
        p_min_mw: f64,
        cost_per_mwh: f64,
        archetype: &str,
        in_service: bool,
        baseline_mw: Option<f64>,
        reserve_offers: Option<Vec<(String, f64, f64)>>,
        qualifications: Option<HashMap<String, bool>>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        if !net.buses.iter().any(|b| b.number == bus) {
            return Err(NetworkError::new_err(format!("Bus {bus} not found")));
        }
        let mut load = parse_dispatchable_load_archetype(
            archetype,
            bus,
            p_sched_mw,
            q_sched_mvar,
            p_min_mw,
            cost_per_mwh,
            net.base_mva,
        )?;
        load.in_service = in_service;
        load.baseline_mw = baseline_mw;
        load.reserve_offers = reserve_offers
            .unwrap_or_default()
            .into_iter()
            .map(
                |(product_id, capacity_mw, cost_per_mwh)| surge_network::market::ReserveOffer {
                    product_id,
                    capacity_mw,
                    cost_per_mwh,
                },
            )
            .collect();
        load.qualifications = qualifications.unwrap_or_default();
        net.market_data.dispatchable_loads.push(load);
        Ok(())
    }

    /// Add a dispatchable-load resource from an editable ``DispatchableLoad`` object.
    pub fn add_dispatchable_load_object(
        &mut self,
        load: PyRef<'_, rich_objects::DispatchableLoad>,
    ) -> PyResult<()> {
        self.add_dispatchable_load(
            load.bus,
            load.p_sched_mw,
            load.q_sched_mvar,
            load.pmin_mw,
            load.cost_per_mwh.unwrap_or(0.0),
            &load.archetype,
            load.in_service,
            load.baseline_mw,
            Some(load.reserve_offers.clone()),
            Some(load.qualifications.clone()),
        )?;
        let last_index = self
            .inner
            .market_data
            .dispatchable_loads
            .len()
            .saturating_sub(1);
        let net = Arc::make_mut(&mut self.inner);
        apply_dispatchable_load_object(&load, net, last_index)
    }

    /// Remove a dispatchable-load resource by list index.
    pub fn remove_dispatchable_load(&mut self, index: usize) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        if index >= net.market_data.dispatchable_loads.len() {
            return Err(NetworkError::new_err(format!(
                "Dispatchable load index {index} out of range"
            )));
        }
        net.market_data.dispatchable_loads.remove(index);
        Ok(())
    }

    /// Set a dispatchable-load resource in- or out-of-service by list index.
    pub fn set_dispatchable_load_in_service(
        &mut self,
        index: usize,
        in_service: bool,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let load = net
            .market_data
            .dispatchable_loads
            .get_mut(index)
            .ok_or_else(|| {
                NetworkError::new_err(format!("Dispatchable load index {index} out of range"))
            })?;
        load.in_service = in_service;
        Ok(())
    }

    /// Replace reserve offers on a dispatchable-load resource by list index.
    pub fn set_dispatchable_load_reserve_offers(
        &mut self,
        index: usize,
        reserve_offers: Vec<(String, f64, f64)>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let load = net
            .market_data
            .dispatchable_loads
            .get_mut(index)
            .ok_or_else(|| {
                NetworkError::new_err(format!("Dispatchable load index {index} out of range"))
            })?;
        load.reserve_offers = reserve_offers
            .into_iter()
            .map(
                |(product_id, capacity_mw, cost_per_mwh)| surge_network::market::ReserveOffer {
                    product_id,
                    capacity_mw,
                    cost_per_mwh,
                },
            )
            .collect();
        Ok(())
    }

    /// Replace reserve qualification flags on a dispatchable-load resource by list index.
    pub fn set_dispatchable_load_qualifications(
        &mut self,
        index: usize,
        qualifications: HashMap<String, bool>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let load = net
            .market_data
            .dispatchable_loads
            .get_mut(index)
            .ok_or_else(|| {
                NetworkError::new_err(format!("Dispatchable load index {index} out of range"))
            })?;
        load.qualifications = qualifications;
        Ok(())
    }

    /// Apply the fields of an editable ``DispatchableLoad`` object back onto the network.
    pub fn update_dispatchable_load_object(
        &mut self,
        load: PyRef<'_, rich_objects::DispatchableLoad>,
    ) -> PyResult<()> {
        let index = load.index.ok_or_else(|| {
            PyValueError::new_err(
                "DispatchableLoad.update requires an object sourced from the network or an explicit index",
            )
        })?;
        let net = Arc::make_mut(&mut self.inner);
        apply_dispatchable_load_object(&load, net, index)
    }

    // -----------------------------------------------------------------------
    // Generator reactive limits (#13)
    // -----------------------------------------------------------------------

    /// Set the reactive power capability limits for a generator.
    ///
    /// These limits define the operating range in the P-Q capability diagram
    /// (D-curve) at rated active power.
    ///
    /// Args:
    ///   id:        Canonical generator identifier.
    ///   qmin_mvar: Minimum reactive power output (MVAr), typically ≤ 0.
    ///   qmax_mvar: Maximum reactive power output (MVAr), typically ≥ 0.
    ///
    /// Raises:
    ///   NetworkError: if no matching generator is found.
    pub fn set_generator_reactive_limits(
        &mut self,
        id: &str,
        qmin_mvar: f64,
        qmax_mvar: f64,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let generator = find_generator_mut_by_id(&mut net.generators, id)?;
        generator.qmin = qmin_mvar;
        generator.qmax = qmax_mvar;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Branch thermal ratings (#13)
    // -----------------------------------------------------------------------

    /// Set all three thermal ratings (A/B/C) for a branch simultaneously.
    ///
    /// - Rate A: Long-term emergency rating (normal operations).
    /// - Rate B: Short-term emergency rating (NERC post-contingency).
    /// - Rate C: Operator-defined emergency rating.
    ///
    /// Args:
    ///   from_bus:    From-bus number.
    ///   to_bus:      To-bus number.
    ///   rate_a_mva:  Long-term continuous rating (MVA).
    ///   rate_b_mva:  Short-term emergency rating (MVA).
    ///   rate_c_mva:  Operator emergency rating (MVA).
    ///   circuit:     Circuit identifier integer (default 1).
    ///
    /// Raises:
    ///   NetworkError: if the branch is not found.
    #[pyo3(signature = (from_bus, to_bus, rate_a_mva, rate_b_mva, rate_c_mva, circuit=1))]
    pub fn set_branch_ratings(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        rate_a_mva: f64,
        rate_b_mva: f64,
        rate_c_mva: f64,
        circuit: i64,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let c = circuit.to_string();
        let idx = find_branch_str(&net.branches, from_bus, to_bus, &c).ok_or_else(|| {
            NetworkError::new_err(format!("Branch {from_bus}-{to_bus} circuit {c} not found"))
        })?;
        net.branches[idx].rating_a_mva = rate_a_mva;
        net.branches[idx].rating_b_mva = rate_b_mva;
        net.branches[idx].rating_c_mva = rate_c_mva;
        Ok(())
    }

    /// Set the per-side (asymmetric) shunt admittance additions for a branch.
    ///
    /// GO Competition Challenge 3 §4.8 eqs (148)-(151) allow AC lines and
    /// transformers to carry distinct shunt-to-ground components at the
    /// from and to terminals (via `additional_shunt = 1` + the four
    /// `g_fr/b_fr/g_to/b_to` fields in the JSON). These are additions
    /// on top of the symmetric `b/2` and `g_pi/2` pi-model split stored
    /// in `Branch::b`/`Branch::g_pi`. Default values are 0.0, preserving
    /// the symmetric pi-model for branches without per-side data.
    ///
    /// Args:
    ///   from_bus:  From-bus number.
    ///   to_bus:    To-bus number.
    ///   g_from_pu: From-terminal shunt conductance (pu).
    ///   b_from_pu: From-terminal shunt susceptance (pu).
    ///   g_to_pu:   To-terminal shunt conductance (pu).
    ///   b_to_pu:   To-terminal shunt susceptance (pu).
    ///   circuit:   Circuit identifier integer (default 1).
    ///
    /// Raises:
    ///   NetworkError: if the branch is not found.
    #[pyo3(signature = (
        from_bus,
        to_bus,
        g_from_pu,
        b_from_pu,
        g_to_pu,
        b_to_pu,
        circuit=1,
    ))]
    pub fn set_branch_additional_shunt(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        g_from_pu: f64,
        b_from_pu: f64,
        g_to_pu: f64,
        b_to_pu: f64,
        circuit: i64,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let c = circuit.to_string();
        let idx = find_branch_str(&net.branches, from_bus, to_bus, &c).ok_or_else(|| {
            NetworkError::new_err(format!("Branch {from_bus}-{to_bus} circuit {c} not found"))
        })?;
        net.branches[idx].g_shunt_from = g_from_pu;
        net.branches[idx].b_shunt_from = b_from_pu;
        net.branches[idx].g_shunt_to = g_to_pu;
        net.branches[idx].b_shunt_to = b_to_pu;
        Ok(())
    }

    /// Set the branch switching transition costs (`c^su_j`, `c^sd_j`).
    ///
    /// GO Competition Challenge 3 §4.4.6 eqs (62)-(63) price the binary
    /// startup and shutdown indicators `u^su_jt`/`u^sd_jt` at a fixed
    /// cost per transition for every `j ∈ J^pr,cs,ac` — including AC
    /// branches. The GO C3 data format surfaces these as the
    /// `connection_cost` and `disconnection_cost` fields on AC line
    /// and transformer records. The default is `0.0`, which matches
    /// the behaviour of non-GO datasets that do not carry branch
    /// transition costs.
    ///
    /// The stored costs are consulted by SCUC only when
    /// `allow_branch_switching = true`; under the default
    /// `AllowSwitching = 0` mode the branch commitment columns are
    /// pinned to `u^on,0_j` and no transitions occur.
    ///
    /// Args:
    ///   from_bus:    From-bus number.
    ///   to_bus:      To-bus number.
    ///   startup:     Cost ($) per open-to-closed transition.
    ///   shutdown:    Cost ($) per closed-to-open transition.
    ///   circuit:     Circuit identifier integer (default 1).
    ///
    /// Raises:
    ///   NetworkError: if the branch is not found.
    #[pyo3(signature = (from_bus, to_bus, startup, shutdown, circuit=1))]
    pub fn set_branch_transition_costs(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        startup: f64,
        shutdown: f64,
        circuit: i64,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let c = circuit.to_string();
        let idx = find_branch_str(&net.branches, from_bus, to_bus, &c).ok_or_else(|| {
            NetworkError::new_err(format!("Branch {from_bus}-{to_bus} circuit {c} not found"))
        })?;
        net.branches[idx].cost_startup = startup;
        net.branches[idx].cost_shutdown = shutdown;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // LCC HVDC lines (#13)
    // -----------------------------------------------------------------------

    /// Add a two-terminal LCC-HVDC line to the network.
    ///
    /// Creates a line-commutated converter (thyristor bridge) DC link between a
    /// rectifier bus (AC → DC) and an inverter bus (DC → AC).
    ///
    /// The converter terminals are populated with default firing-angle/tap
    /// ranges (α_max=90°, α_min=5°, tap range 0.9–1.1) which are suitable for
    /// most studies. Use the ``LccHvdcLink`` data returned by ``network.hvdc.links`` for
    /// fine-grained control.
    ///
    /// When ``p_dc_min_mw < p_dc_max_mw`` the joint AC-DC OPF treats this
    /// link's DC power as an NLP decision variable bounded by the given
    /// range; otherwise (the default) it's pinned at ``setvl_mw`` and the
    /// sequential AC-DC iteration handles it.
    ///
    /// Args:
    ///   name:          Unique name for the DC link.
    ///   rect_bus:      AC bus number of the rectifier (AC → DC).
    ///   inv_bus:       AC bus number of the inverter (DC → AC).
    ///   setvl_mw:      Scheduled DC power (MW, positive = rectifier to inverter).
    ///   vschd_kv:      Scheduled DC voltage (kV).
    ///   rdc:           DC circuit resistance (Ω, default 0.0).
    ///   p_dc_min_mw:   Minimum DC power for joint AC-DC OPF (MW, default 0.0).
    ///   p_dc_max_mw:   Maximum DC power for joint AC-DC OPF (MW, default 0.0).
    ///
    /// Raises:
    ///   NetworkError: if ``rect_bus`` or ``inv_bus`` does not exist, or
    ///     if ``p_dc_min_mw > p_dc_max_mw``.
    #[pyo3(signature = (name, rect_bus, inv_bus, setvl_mw, vschd_kv, rdc=0.0, p_dc_min_mw=0.0, p_dc_max_mw=0.0))]
    pub fn add_lcc_dc_line(
        &mut self,
        name: &str,
        rect_bus: u32,
        inv_bus: u32,
        setvl_mw: f64,
        vschd_kv: f64,
        rdc: f64,
        p_dc_min_mw: f64,
        p_dc_max_mw: f64,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let bm = net.bus_index_map();
        if !bm.contains_key(&rect_bus) {
            return Err(NetworkError::new_err(format!(
                "Rectifier bus {rect_bus} not found"
            )));
        }
        if !bm.contains_key(&inv_bus) {
            return Err(NetworkError::new_err(format!(
                "Inverter bus {inv_bus} not found"
            )));
        }
        if p_dc_min_mw > p_dc_max_mw {
            return Err(NetworkError::new_err(format!(
                "LCC DC line '{name}': p_dc_min_mw ({p_dc_min_mw}) must be \
                 ≤ p_dc_max_mw ({p_dc_max_mw})"
            )));
        }
        let mut line = LccHvdcLink {
            name: name.to_string(),
            scheduled_setpoint: setvl_mw,
            scheduled_voltage_kv: vschd_kv,
            resistance_ohm: rdc,
            p_dc_min_mw,
            p_dc_max_mw,
            ..Default::default()
        };
        line.rectifier.bus = rect_bus;
        line.inverter.bus = inv_bus;
        net.hvdc.push_lcc_link(line);
        Ok(())
    }

    /// Remove an LCC-HVDC line by name.
    ///
    /// Args:
    ///   name: Name of the DC line to remove.
    ///
    /// Raises:
    ///   NetworkError: if no DC line with that name is found.
    pub fn remove_lcc_dc_line(&mut self, name: &str) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let idx = net
            .hvdc
            .links
            .iter()
            .position(|link| link.as_lcc().is_some_and(|line| line.name == name))
            .ok_or_else(|| NetworkError::new_err(format!("LCC DC line '{name}' not found")))?;
        net.hvdc.links.remove(idx);
        Ok(())
    }

    /// Add an LCC-HVDC line from an editable ``LccHvdcLink`` object.
    pub fn add_dc_line_object(&mut self, line: PyRef<'_, rich_objects::DcLine>) -> PyResult<()> {
        self.add_lcc_dc_line(
            &line.name,
            line.rectifier_bus,
            line.inverter_bus,
            line.scheduled_setpoint,
            line.scheduled_voltage_kv,
            line.resistance_ohm,
            line.p_dc_min_mw,
            line.p_dc_max_mw,
        )?;
        self.update_dc_line_object(line)
    }

    /// Apply the fields of an editable ``LccHvdcLink`` object back onto the network.
    pub fn update_dc_line_object(&mut self, line: PyRef<'_, rich_objects::DcLine>) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let target = net
            .hvdc
            .links
            .iter_mut()
            .find_map(|existing| {
                existing
                    .as_lcc_mut()
                    .filter(|existing| existing.name == line.name)
            })
            .ok_or_else(|| {
                NetworkError::new_err(format!("LCC DC line '{}' not found", line.name))
            })?;
        if line.p_dc_min_mw > line.p_dc_max_mw {
            return Err(NetworkError::new_err(format!(
                "LCC DC line '{}': p_dc_min_mw ({}) must be ≤ p_dc_max_mw ({})",
                line.name, line.p_dc_min_mw, line.p_dc_max_mw
            )));
        }
        target.scheduled_setpoint = line.scheduled_setpoint;
        target.scheduled_voltage_kv = line.scheduled_voltage_kv;
        target.resistance_ohm = line.resistance_ohm;
        target.rectifier.bus = line.rectifier_bus;
        target.inverter.bus = line.inverter_bus;
        target.p_dc_min_mw = line.p_dc_min_mw;
        target.p_dc_max_mw = line.p_dc_max_mw;
        target.mode = if line.in_service {
            surge_network::network::LccHvdcControlMode::PowerControl
        } else {
            surge_network::network::LccHvdcControlMode::Blocked
        };
        target.rectifier.in_service = line.in_service;
        target.inverter.in_service = line.in_service;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // VSC HVDC lines (#13)
    // -----------------------------------------------------------------------

    /// Add a two-terminal VSC-HVDC link to the network.
    ///
    /// Creates a voltage-source converter (IGBT) DC link.  Each converter can
    /// independently control its reactive power or AC voltage setpoint.
    ///
    /// Args:
    ///   name:             Unique name for the VSC link.
    ///   bus_f:            AC bus number of the from-terminal converter.
    ///   bus_t:            AC bus number of the to-terminal converter.
    ///   mw_setpoint:      Scheduled DC power at the from-terminal (MW, positive = from→to).
    ///   ac_vm_f:          AC voltage setpoint at the from-terminal (p.u., default 1.0).
    ///                     Activates ``AcVoltage`` control mode on the from-converter.
    ///   ac_vm_t:          AC voltage setpoint at the to-terminal (p.u., default 1.0).
    ///                     Activates ``AcVoltage`` control mode on the to-converter.
    ///   q_min_mvar:       Minimum reactive injection at each converter (MVAr, default -9999).
    ///   q_max_mvar:       Maximum reactive injection at each converter (MVAr, default 9999).
    ///
    /// Raises:
    ///   NetworkError: if ``bus_f`` or ``bus_t`` does not exist.
    #[pyo3(
        signature = (name, bus_f, bus_t, mw_setpoint,
                     ac_vm_f=1.0, ac_vm_t=1.0,
                     q_min_mvar=-9999.0, q_max_mvar=9999.0)
    )]
    pub fn add_vsc_dc_line(
        &mut self,
        name: &str,
        bus_f: u32,
        bus_t: u32,
        mw_setpoint: f64,
        ac_vm_f: f64,
        ac_vm_t: f64,
        q_min_mvar: f64,
        q_max_mvar: f64,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let bm = net.bus_index_map();
        if !bm.contains_key(&bus_f) {
            return Err(NetworkError::new_err(format!("Bus {bus_f} not found")));
        }
        if !bm.contains_key(&bus_t) {
            return Err(NetworkError::new_err(format!("Bus {bus_t} not found")));
        }
        let mut vsc = VscHvdcLink {
            name: name.to_string(),
            mode: VscHvdcControlMode::PowerControl,
            ..Default::default()
        };

        vsc.converter1.bus = bus_f;
        vsc.converter1.control_mode = VscConverterAcControlMode::AcVoltage;
        vsc.converter1.dc_setpoint = mw_setpoint;
        vsc.converter1.ac_setpoint = ac_vm_f;
        vsc.converter1.q_min_mvar = q_min_mvar;
        vsc.converter1.q_max_mvar = q_max_mvar;

        vsc.converter2.bus = bus_t;
        vsc.converter2.control_mode = VscConverterAcControlMode::AcVoltage;
        vsc.converter2.dc_setpoint = -mw_setpoint; // absorbs equal power at to-terminal
        vsc.converter2.ac_setpoint = ac_vm_t;
        vsc.converter2.q_min_mvar = q_min_mvar;
        vsc.converter2.q_max_mvar = q_max_mvar;

        net.hvdc.push_vsc_link(vsc);
        Ok(())
    }

    /// Remove a VSC-HVDC link by name.
    ///
    /// Args:
    ///   name: Name of the VSC link to remove.
    ///
    /// Raises:
    ///   NetworkError: if no VSC link with that name is found.
    pub fn remove_vsc_dc_line(&mut self, name: &str) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let idx = net
            .hvdc
            .links
            .iter()
            .position(|link| link.as_vsc().is_some_and(|line| line.name == name))
            .ok_or_else(|| NetworkError::new_err(format!("VSC DC line '{name}' not found")))?;
        net.hvdc.links.remove(idx);
        Ok(())
    }

    /// Add a VSC-HVDC line from an editable ``VscHvdcLink`` object.
    pub fn add_vsc_dc_line_object(
        &mut self,
        line: PyRef<'_, rich_objects::VscDcLine>,
    ) -> PyResult<()> {
        self.add_vsc_dc_line(
            &line.name,
            line.converter1_bus,
            line.converter2_bus,
            line.p_mw,
            1.0,
            1.0,
            line.q1_min_mvar.min(line.q2_min_mvar),
            line.q1_max_mvar.max(line.q2_max_mvar),
        )?;
        self.update_vsc_dc_line_object(line)
    }

    /// Apply the fields of an editable ``VscHvdcLink`` object back onto the network.
    pub fn update_vsc_dc_line_object(
        &mut self,
        line: PyRef<'_, rich_objects::VscDcLine>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let target = net
            .hvdc
            .links
            .iter_mut()
            .find_map(|existing| {
                existing
                    .as_vsc_mut()
                    .filter(|existing| existing.name == line.name)
            })
            .ok_or_else(|| {
                NetworkError::new_err(format!("VSC DC line '{}' not found", line.name))
            })?;
        target.mode = if line.in_service {
            VscHvdcControlMode::PowerControl
        } else {
            VscHvdcControlMode::Blocked
        };
        target.resistance_ohm = line.resistance_ohm;
        target.converter1.bus = line.converter1_bus;
        target.converter1.dc_setpoint = line.p_mw;
        target.converter1.loss_constant_mw = line.loss_a_mw / 2.0;
        target.converter1.loss_linear = line.loss_linear / 2.0;
        target.converter1.q_min_mvar = line.q1_min_mvar;
        target.converter1.q_max_mvar = line.q1_max_mvar;
        target.converter1.in_service = line.in_service;
        target.converter2.bus = line.converter2_bus;
        target.converter2.dc_setpoint = -line.p_mw;
        target.converter2.loss_constant_mw = line.loss_a_mw / 2.0;
        target.converter2.loss_linear = line.loss_linear / 2.0;
        target.converter2.q_min_mvar = line.q2_min_mvar;
        target.converter2.q_max_mvar = line.q2_max_mvar;
        target.converter2.in_service = line.in_service;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // FACTS devices (#13)
    // -----------------------------------------------------------------------

    /// Add a FACTS device to the network.
    ///
    /// Supported device types (selected by ``mode``):
    ///
    /// - ``"ShuntOnly"``  — SVC or STATCOM at ``bus_from``.
    ///   Controls bus voltage by injecting/absorbing reactive power.
    /// - ``"SeriesOnly"``  — TCSC between ``bus_from`` and ``bus_to``.
    ///   Modifies the branch reactance by ``−linx`` p.u.
    /// - ``"ShuntSeries"``  — UPFC: combined shunt + series device.
    /// - ``"SeriesPowerControl"``  — TCSC with active power flow target.
    /// - ``"ImpedanceModulation"``  — Direct branch impedance modulation.
    ///
    /// Args:
    ///   name:   Unique device name.
    ///   bus_from:  Shunt connection bus (primary bus).
    ///   bus_to:  Series connection bus (remote bus; 0 for shunt-only devices).
    ///   mode:   Operating mode string (default ``"ShuntOnly"``).
    ///   v_set:  Voltage setpoint at ``bus_from`` (p.u., default 1.0).
    ///   q_max:  Maximum reactive injection magnitude (MVAr, default 9999).
    ///   linx:   Series reactance modification in p.u. (default 0.0).
    ///
    /// Raises:
    ///   ValueError: if ``mode`` is not one of the recognised values.
    ///   NetworkError: if ``bus_from`` does not exist.
    #[pyo3(
        signature = (name, bus_from, bus_to=0, mode="ShuntOnly",
                     v_set=1.0, q_max=9999.0, linx=0.0)
    )]
    pub fn add_facts_device(
        &mut self,
        name: &str,
        bus_from: u32,
        bus_to: u32,
        mode: &str,
        v_set: f64,
        q_max: f64,
        linx: f64,
    ) -> PyResult<()> {
        let facts_mode = parse_facts_mode(mode)?;
        let net = Arc::make_mut(&mut self.inner);
        let bm = net.bus_index_map();
        if !bm.contains_key(&bus_from) {
            return Err(NetworkError::new_err(format!("Bus {bus_from} not found")));
        }
        if bus_to != 0 && !bm.contains_key(&bus_to) {
            return Err(NetworkError::new_err(format!("Bus {bus_to} not found")));
        }
        net.facts_devices.push(FactsDevice {
            name: name.to_string(),
            bus_from,
            bus_to,
            mode: facts_mode,
            p_setpoint_mw: 0.0,
            q_setpoint_mvar: 0.0,
            voltage_setpoint_pu: v_set,
            q_max,
            series_reactance_pu: linx,
            in_service: facts_mode.in_service(),
            ..FactsDevice::default()
        });
        Ok(())
    }

    /// Remove a FACTS device by name.
    ///
    /// Args:
    ///   name: Name of the FACTS device to remove.
    ///
    /// Raises:
    ///   NetworkError: if no FACTS device with that name is found.
    pub fn remove_facts_device(&mut self, name: &str) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let idx = net
            .facts_devices
            .iter()
            .position(|f| f.name == name)
            .ok_or_else(|| NetworkError::new_err(format!("FACTS device '{name}' not found")))?;
        net.facts_devices.remove(idx);
        Ok(())
    }

    /// Change the operating mode of an existing FACTS device.
    ///
    /// Valid modes: ``"OutOfService"``, ``"SeriesOnly"``, ``"ShuntOnly"``,
    /// ``"ShuntSeries"``, ``"SeriesPowerControl"``, ``"ImpedanceModulation"``.
    ///
    /// Args:
    ///   name: Name of the FACTS device to modify.
    ///   mode: New operating mode string.
    ///
    /// Raises:
    ///   ValueError: if ``mode`` is not recognised.
    ///   NetworkError: if the device is not found.
    pub fn set_facts_mode(&mut self, name: &str, mode: &str) -> PyResult<()> {
        let facts_mode = parse_facts_mode(mode)?;
        let net = Arc::make_mut(&mut self.inner);
        let device = net
            .facts_devices
            .iter_mut()
            .find(|f| f.name == name)
            .ok_or_else(|| NetworkError::new_err(format!("FACTS device '{name}' not found")))?;
        device.mode = facts_mode;
        device.in_service = facts_mode.in_service();
        Ok(())
    }

    /// Add a FACTS device from an editable ``FactsDevice`` object.
    pub fn add_facts_device_object(
        &mut self,
        device: PyRef<'_, rich_objects::FactsDevice>,
    ) -> PyResult<()> {
        self.add_facts_device(
            &device.name,
            device.bus_from,
            device.bus_to,
            &device.mode,
            device.v_set_pu,
            device.q_max_mvar,
            device.linx_pu,
        )?;
        self.update_facts_device_object(device)
    }

    /// Apply the fields of an editable ``FactsDevice`` object back onto the network.
    pub fn update_facts_device_object(
        &mut self,
        device: PyRef<'_, rich_objects::FactsDevice>,
    ) -> PyResult<()> {
        let facts_mode = parse_facts_mode(&device.mode)?;
        let net = Arc::make_mut(&mut self.inner);
        let target = net
            .facts_devices
            .iter_mut()
            .find(|existing| existing.name == device.name)
            .ok_or_else(|| {
                NetworkError::new_err(format!("FACTS device '{}' not found", device.name))
            })?;
        target.bus_from = device.bus_from;
        target.bus_to = device.bus_to;
        target.mode = facts_mode;
        target.p_setpoint_mw = device.p_des_mw;
        target.q_setpoint_mvar = device.q_des_mvar;
        target.voltage_setpoint_pu = device.v_set_pu;
        target.q_max = device.q_max_mvar;
        target.series_reactance_pu = device.linx_pu;
        target.in_service = device.in_service && facts_mode.in_service();
        Ok(())
    }

    /// Add an area interchange record from an editable ``AreaSchedule`` object.
    pub fn add_area_schedule_object(
        &mut self,
        schedule: PyRef<'_, rich_objects::AreaSchedule>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        if net
            .area_schedules
            .iter()
            .any(|existing| existing.number == schedule.area)
        {
            return Err(NetworkError::new_err(format!(
                "Area schedule {} already exists",
                schedule.area
            )));
        }
        net.area_schedules.push(CoreAreaSchedule {
            number: schedule.area,
            slack_bus: schedule.slack_bus,
            p_desired_mw: schedule.p_desired_mw,
            p_tolerance_mw: schedule.p_tolerance_mw,
            name: schedule.name.clone(),
        });
        Ok(())
    }

    /// Apply an editable ``AreaSchedule`` object back onto the network.
    pub fn update_area_schedule_object(
        &mut self,
        schedule: PyRef<'_, rich_objects::AreaSchedule>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let target = net
            .area_schedules
            .iter_mut()
            .find(|existing| existing.number == schedule.area)
            .ok_or_else(|| {
                NetworkError::new_err(format!("Area schedule {} not found", schedule.area))
            })?;
        target.slack_bus = schedule.slack_bus;
        target.p_desired_mw = schedule.p_desired_mw;
        target.p_tolerance_mw = schedule.p_tolerance_mw;
        target.name = schedule.name.clone();
        Ok(())
    }

    /// Add a breaker rating from an editable ``BreakerRating`` object.
    pub fn add_breaker_rating_object(
        &mut self,
        rating: PyRef<'_, rich_objects::BreakerRating>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        net.breaker_ratings.push(CoreBreakerRating {
            bus: rating.bus,
            name: rating.name.clone(),
            rated_kv: rating.rated_kv,
            interrupting_ka: rating.interrupting_ka,
            momentary_ka: rating.momentary_ka,
            clearing_time_cycles: rating.clearing_time_cycles,
            in_service: rating.in_service,
        });
        Ok(())
    }

    /// Apply an editable ``BreakerRating`` object back onto the network.
    pub fn update_breaker_rating_object(
        &mut self,
        rating: PyRef<'_, rich_objects::BreakerRating>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let target = net
            .breaker_ratings
            .iter_mut()
            .find(|existing| existing.bus == rating.bus && existing.name == rating.name)
            .ok_or_else(|| {
                NetworkError::new_err(format!(
                    "Breaker rating '{}' at bus {} not found",
                    rating.name, rating.bus
                ))
            })?;
        target.rated_kv = rating.rated_kv;
        target.interrupting_ka = rating.interrupting_ka;
        target.momentary_ka = rating.momentary_ka;
        target.clearing_time_cycles = rating.clearing_time_cycles;
        target.in_service = rating.in_service;
        Ok(())
    }

    /// Add a fixed shunt from an editable ``FixedShunt`` object.
    pub fn add_fixed_shunt_object(
        &mut self,
        shunt: PyRef<'_, rich_objects::FixedShunt>,
    ) -> PyResult<()> {
        let shunt_type = parse_shunt_type(&shunt.shunt_type)?;
        let net = Arc::make_mut(&mut self.inner);
        net.fixed_shunts.push(CoreFixedShunt {
            bus: shunt.bus,
            id: shunt.id.clone(),
            shunt_type,
            g_mw: shunt.g_mw,
            b_mvar: shunt.b_mvar,
            in_service: shunt.in_service,
            rated_kv: shunt.rated_kv,
            rated_mvar: shunt.rated_mvar,
        });
        Ok(())
    }

    /// Apply an editable ``FixedShunt`` object back onto the network.
    pub fn update_fixed_shunt_object(
        &mut self,
        shunt: PyRef<'_, rich_objects::FixedShunt>,
    ) -> PyResult<()> {
        let shunt_type = parse_shunt_type(&shunt.shunt_type)?;
        let net = Arc::make_mut(&mut self.inner);
        let target = net
            .fixed_shunts
            .iter_mut()
            .find(|existing| existing.bus == shunt.bus && existing.id == shunt.id)
            .ok_or_else(|| {
                NetworkError::new_err(format!(
                    "Fixed shunt '{}' at bus {} not found",
                    shunt.id, shunt.bus
                ))
            })?;
        target.shunt_type = shunt_type;
        target.g_mw = shunt.g_mw;
        target.b_mvar = shunt.b_mvar;
        target.in_service = shunt.in_service;
        target.rated_kv = shunt.rated_kv;
        target.rated_mvar = shunt.rated_mvar;
        Ok(())
    }

    /// Add a reserve zone from an editable ``ReserveZone`` object.
    pub fn add_reserve_zone_object(
        &mut self,
        zone: PyRef<'_, rich_objects::ReserveZone>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        if net
            .market_data
            .reserve_zones
            .iter()
            .any(|existing| existing.name == zone.name)
        {
            return Err(NetworkError::new_err(format!(
                "Reserve zone '{}' already exists",
                zone.name
            )));
        }
        net.market_data
            .reserve_zones
            .push(surge_network::market::ReserveZone {
                name: zone.name.clone(),
                zonal_requirements: zone
                    .zonal_requirements
                    .iter()
                    .map(
                        |(zone_id, product_id, requirement_mw, participant_bus_numbers)| {
                            surge_network::market::ZonalReserveRequirement {
                                zone_id: *zone_id,
                                product_id: product_id.clone(),
                                requirement_mw: *requirement_mw,
                                per_period_mw: None,
                                shortfall_cost_per_unit: None,
                                served_dispatchable_load_coefficient: None,
                                largest_generator_dispatch_coefficient: None,
                                participant_bus_numbers: participant_bus_numbers.clone(),
                            }
                        },
                    )
                    .collect(),
            });
        Ok(())
    }

    /// Apply an editable ``ReserveZone`` object back onto the network.
    pub fn update_reserve_zone_object(
        &mut self,
        zone: PyRef<'_, rich_objects::ReserveZone>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let target = net
            .market_data
            .reserve_zones
            .iter_mut()
            .find(|existing| existing.name == zone.name)
            .ok_or_else(|| {
                NetworkError::new_err(format!("Reserve zone '{}' not found", zone.name))
            })?;
        target.zonal_requirements = zone
            .zonal_requirements
            .iter()
            .map(
                |(zone_id, product_id, requirement_mw, participant_bus_numbers)| {
                    surge_network::market::ZonalReserveRequirement {
                        zone_id: *zone_id,
                        product_id: product_id.clone(),
                        requirement_mw: *requirement_mw,
                        per_period_mw: None,
                        shortfall_cost_per_unit: None,
                        served_dispatchable_load_coefficient: None,
                        largest_generator_dispatch_coefficient: None,
                        participant_bus_numbers: participant_bus_numbers.clone(),
                    }
                },
            )
            .collect();
        Ok(())
    }

    /// Add a pumped-hydro unit from an editable ``PumpedHydroUnit`` object.
    pub fn add_pumped_hydro_unit_object(
        &mut self,
        unit: PyRef<'_, rich_objects::PumpedHydroUnit>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        net.market_data
            .pumped_hydro_units
            .push(CorePumpedHydroUnit {
                name: unit.name.clone(),
                generator: GeneratorRef {
                    bus: unit.generator_bus,
                    id: unit.generator_id.clone(),
                },
                variable_speed: unit.variable_speed,
                pump_mw_fixed: unit.pump_mw_fixed,
                pump_mw_min: unit.pump_mw_min,
                pump_mw_max: unit.pump_mw_max,
                mode_transition_min: unit.mode_transition_min,
                condenser_capable: unit.condenser_capable,
                forbidden_zone: None,
                upper_reservoir_mwh: unit.upper_reservoir_mwh,
                lower_reservoir_mwh: unit.lower_reservoir_mwh,
                soc_initial_mwh: unit.soc_initial_mwh,
                soc_min_mwh: unit.soc_min_mwh,
                soc_max_mwh: unit.soc_max_mwh,
                efficiency_generate: unit.efficiency_generate,
                efficiency_pump: unit.efficiency_pump,
                head_curve: Vec::new(),
                n_units: unit.n_units,
                shared_penstock_mw_max: unit.shared_penstock_mw_max,
                min_release_mw: unit.min_release_mw,
                ramp_rate_mw_per_min: unit.ramp_rate_mw_per_min,
                startup_time_gen_min: unit.startup_time_gen_min,
                startup_time_pump_min: unit.startup_time_pump_min,
                startup_cost: unit.startup_cost,
                reserve_offers: unit
                    .reserve_offers
                    .iter()
                    .map(|(product_id, capacity_mw, cost_per_mwh)| {
                        surge_network::market::ReserveOffer {
                            product_id: product_id.clone(),
                            capacity_mw: *capacity_mw,
                            cost_per_mwh: *cost_per_mwh,
                        }
                    })
                    .collect(),
                qualifications: unit.qualifications.clone(),
            });
        Ok(())
    }

    /// Apply an editable ``PumpedHydroUnit`` object back onto the network.
    pub fn update_pumped_hydro_unit_object(
        &mut self,
        unit: PyRef<'_, rich_objects::PumpedHydroUnit>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let target = net
            .market_data
            .pumped_hydro_units
            .iter_mut()
            .find(|existing| {
                existing.name == unit.name
                    && existing.generator.bus == unit.generator_bus
                    && existing.generator.id == unit.generator_id
            })
            .ok_or_else(|| {
                NetworkError::new_err(format!(
                    "Pumped hydro unit '{}' generator=({}, '{}') not found",
                    unit.name, unit.generator_bus, unit.generator_id
                ))
            })?;
        target.generator = GeneratorRef {
            bus: unit.generator_bus,
            id: unit.generator_id.clone(),
        };
        target.variable_speed = unit.variable_speed;
        target.pump_mw_fixed = unit.pump_mw_fixed;
        target.pump_mw_min = unit.pump_mw_min;
        target.pump_mw_max = unit.pump_mw_max;
        target.mode_transition_min = unit.mode_transition_min;
        target.condenser_capable = unit.condenser_capable;
        target.upper_reservoir_mwh = unit.upper_reservoir_mwh;
        target.lower_reservoir_mwh = unit.lower_reservoir_mwh;
        target.soc_initial_mwh = unit.soc_initial_mwh;
        target.soc_min_mwh = unit.soc_min_mwh;
        target.soc_max_mwh = unit.soc_max_mwh;
        target.efficiency_generate = unit.efficiency_generate;
        target.efficiency_pump = unit.efficiency_pump;
        target.n_units = unit.n_units;
        target.shared_penstock_mw_max = unit.shared_penstock_mw_max;
        target.min_release_mw = unit.min_release_mw;
        target.ramp_rate_mw_per_min = unit.ramp_rate_mw_per_min;
        target.startup_time_gen_min = unit.startup_time_gen_min;
        target.startup_time_pump_min = unit.startup_time_pump_min;
        target.startup_cost = unit.startup_cost;
        target.reserve_offers = unit
            .reserve_offers
            .iter()
            .map(
                |(product_id, capacity_mw, cost_per_mwh)| surge_network::market::ReserveOffer {
                    product_id: product_id.clone(),
                    capacity_mw: *capacity_mw,
                    cost_per_mwh: *cost_per_mwh,
                },
            )
            .collect();
        target.qualifications = unit.qualifications.clone();
        Ok(())
    }

    /// Add a combined-cycle plant from an editable ``CombinedCyclePlant`` object.
    pub fn add_combined_cycle_plant_object(
        &mut self,
        plant: PyRef<'_, rich_objects::CombinedCyclePlant>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        if net
            .market_data
            .combined_cycle_plants
            .iter()
            .any(|existing| existing.name == plant.name)
        {
            return Err(NetworkError::new_err(format!(
                "Combined-cycle plant '{}' already exists",
                plant.name
            )));
        }
        net.market_data
            .combined_cycle_plants
            .push(CoreCombinedCyclePlant {
                id: String::new(),
                name: plant.name.clone(),
                configs: plant
                    .configs
                    .iter()
                    .map(|config| CoreCombinedCycleConfig {
                        name: config.name.clone(),
                        gen_indices: config.gen_indices.clone(),
                        p_min_mw: config.p_min_mw,
                        p_max_mw: config.p_max_mw,
                        heat_rate_curve: Vec::new(),
                        energy_offer: None,
                        ramp_up_curve: Vec::new(),
                        ramp_down_curve: Vec::new(),
                        no_load_cost: 0.0,
                        min_up_time_hr: config.min_up_time_hr,
                        min_down_time_hr: config.min_down_time_hr,
                        reserve_offers: Vec::new(),
                        qualifications: HashMap::new(),
                    })
                    .collect(),
                transitions: plant
                    .transitions
                    .iter()
                    .map(|transition| CoreCombinedCycleTransition {
                        from_config: transition.from_config.clone(),
                        to_config: transition.to_config.clone(),
                        transition_time_min: transition.transition_time_min,
                        transition_cost: transition.transition_cost,
                        online_transition: transition.online_transition,
                    })
                    .collect(),
                active_config: plant.active_config.clone(),
                hours_in_config: plant.hours_in_config,
                duct_firing_capable: plant.duct_firing_capable,
            });
        Ok(())
    }

    /// Apply an editable ``CombinedCyclePlant`` object back onto the network.
    pub fn update_combined_cycle_plant_object(
        &mut self,
        plant: PyRef<'_, rich_objects::CombinedCyclePlant>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        let target = net
            .market_data
            .combined_cycle_plants
            .iter_mut()
            .find(|existing| existing.name == plant.name)
            .ok_or_else(|| {
                NetworkError::new_err(format!("Combined-cycle plant '{}' not found", plant.name))
            })?;
        target.configs = plant
            .configs
            .iter()
            .map(|config| CoreCombinedCycleConfig {
                name: config.name.clone(),
                gen_indices: config.gen_indices.clone(),
                p_min_mw: config.p_min_mw,
                p_max_mw: config.p_max_mw,
                heat_rate_curve: Vec::new(),
                energy_offer: None,
                ramp_up_curve: Vec::new(),
                ramp_down_curve: Vec::new(),
                no_load_cost: 0.0,
                min_up_time_hr: config.min_up_time_hr,
                min_down_time_hr: config.min_down_time_hr,
                reserve_offers: Vec::new(),
                qualifications: HashMap::new(),
            })
            .collect();
        target.transitions = plant
            .transitions
            .iter()
            .map(|transition| CoreCombinedCycleTransition {
                from_config: transition.from_config.clone(),
                to_config: transition.to_config.clone(),
                transition_time_min: transition.transition_time_min,
                transition_cost: transition.transition_cost,
                online_transition: transition.online_transition,
            })
            .collect();
        target.active_config = plant.active_config.clone();
        target.hours_in_config = plant.hours_in_config;
        target.duct_firing_capable = plant.duct_firing_capable;
        Ok(())
    }

    /// Add an outage record from an editable ``OutageEntry`` object.
    pub fn add_outage_entry_object(
        &mut self,
        outage: PyRef<'_, rich_objects::OutageEntry>,
    ) -> PyResult<()> {
        let net = Arc::make_mut(&mut self.inner);
        net.market_data.outage_schedule.push(outage.to_core()?);
        Ok(())
    }

    /// Apply an editable ``OutageEntry`` object back onto the network.
    pub fn update_outage_entry_object(
        &mut self,
        outage: PyRef<'_, rich_objects::OutageEntry>,
    ) -> PyResult<()> {
        let schedule_index = outage.schedule_index.ok_or_else(|| {
            PyValueError::new_err(
                "OutageEntry.update requires an object sourced from the network or an explicit schedule index",
            )
        })?;
        let net = Arc::make_mut(&mut self.inner);
        let target = net
            .market_data
            .outage_schedule
            .get_mut(schedule_index)
            .ok_or_else(|| {
                NetworkError::new_err(format!(
                    "Outage schedule index {schedule_index} out of range"
                ))
            })?;
        *target = outage.to_core()?;
        Ok(())
    }

    #[pyo3(signature = (bus, pz=0.0, pi=0.0, pp=100.0, qz=0.0, qi=0.0, qp=100.0))]
    pub fn set_zip_load(
        &mut self,
        bus: u32,
        pz: f64,
        pi: f64,
        pp: f64,
        qz: f64,
        qi: f64,
        qp: f64,
    ) -> PyResult<()> {
        let p_sum = pz + pi + pp;
        let q_sum = qz + qi + qp;
        if (p_sum - 100.0).abs() > 1e-6 {
            return Err(PyValueError::new_err(format!(
                "P coefficients must sum to 100, got {p_sum}"
            )));
        }
        if (q_sum - 100.0).abs() > 1e-6 {
            return Err(PyValueError::new_err(format!(
                "Q coefficients must sum to 100, got {q_sum}"
            )));
        }
        let net = Arc::make_mut(&mut self.inner);
        // Verify bus exists.
        let bm = net.bus_index_map();
        if !bm.contains_key(&bus) {
            return Err(NetworkError::new_err(format!("Bus {bus} not found")));
        }
        // Set ZIP fractions on every load at this bus (convert % → [0,1]).
        for load in &mut net.loads {
            if load.bus == bus {
                load.zip_p_impedance_frac = pz / 100.0;
                load.zip_p_current_frac = pi / 100.0;
                load.zip_p_power_frac = pp / 100.0;
                load.zip_q_impedance_frac = qz / 100.0;
                load.zip_q_current_frac = qi / 100.0;
                load.zip_q_power_frac = qp / 100.0;
            }
        }
        Ok(())
    }
}
