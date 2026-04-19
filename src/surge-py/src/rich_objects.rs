// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Rich Python wrapper objects for power system network elements.
//!
//! Provides object-oriented access to network data and solution results:
//!
//!   - Static network objects: `Bus`, `Branch`, `Generator`, `Load`
//!   - Power flow solved objects: `BusSolved`, `BranchSolved`, `GenSolved`
//!   - OPF result objects: `BusOpf`, `BranchOpf`, `GenOpf`
//!
//! All objects are value snapshots (data is copied from Rust at construction time).
//! The existing array-based API on `Network` and solution types is fully preserved.

use pyo3::prelude::*;

use crate::exceptions::NetworkError;
use surge_network::Network as CoreNetwork;
use surge_network::market::CostCurve;
use surge_network::market::DispatchableLoad as CoreDispatchableLoad;
use surge_network::market::PumpedHydroUnit as CorePumpedHydroUnit;
use surge_network::market::ReserveZone as CoreReserveZone;
use surge_network::market::{
    CombinedCycleConfig as CoreCombinedCycleConfig, CombinedCyclePlant as CoreCombinedCyclePlant,
    CombinedCycleTransition as CoreCombinedCycleTransition,
};
use surge_network::market::{OutageEntry as CoreOutageEntry, OutageType};
use surge_network::network::breaker::BreakerRating as CoreBreakerRating;
use surge_network::network::{
    AreaSchedule as CoreAreaSchedule, Branch as CoreBranch, Bus as CoreBus, BusType,
    CommitmentStatus, DcBranch as CoreDcBranch, DcBus as CoreDcBus, DcConverter as CoreDcConverter,
    DcGrid as CoreDcGrid, EquipmentRef, FactsDevice as CoreFactsDevice, Generator as CoreGenerator,
    GeneratorRef, LccHvdcLink as CoreDcLine, Load as CoreLoad, StorageDispatchMode,
    StorageParams as CoreStorageParams, SwitchedShuntOpf as CoreSwitchedShuntOpf,
    VscHvdcLink as CoreVscDcLine,
};
use surge_network::network::{FixedShunt as CoreFixedShunt, ShuntType};
use surge_solution::{OpfSolution as CoreOpfSolution, PfSolution as CorePfSolution};

fn storage_dispatch_mode_str(mode: StorageDispatchMode) -> &'static str {
    match mode {
        StorageDispatchMode::CostMinimization => "cost_minimization",
        StorageDispatchMode::OfferCurve => "offer_curve",
        StorageDispatchMode::SelfSchedule => "self_schedule",
    }
}

fn parse_storage_dispatch_mode(value: &str) -> PyResult<StorageDispatchMode> {
    match value {
        "cost_minimization" => Ok(StorageDispatchMode::CostMinimization),
        "offer_curve" => Ok(StorageDispatchMode::OfferCurve),
        "self_schedule" => Ok(StorageDispatchMode::SelfSchedule),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "invalid storage dispatch_mode '{other}'; expected cost_minimization, offer_curve, or self_schedule"
        ))),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Bus
// ─────────────────────────────────────────────────────────────────────────────

/// A bus (node) in the power system network — all static model fields.
///
/// Obtain via `net.buses`, `net.bus(n)`, or `net.slack_bus`.
#[pyclass(name = "Bus", skip_from_py_object)]
#[derive(Clone)]
pub struct Bus {
    /// External bus number (unique identifier).
    #[pyo3(get)]
    pub number: u32,
    /// Bus name.
    #[pyo3(get, set)]
    pub name: String,
    /// Bus type string: "PQ", "PV", "Slack", or "Isolated".
    #[pyo3(get, set)]
    pub type_str: String,
    /// Real power load (MW).
    #[pyo3(get, set)]
    pub pd_mw: f64,
    /// Reactive power load (MVAr).
    #[pyo3(get, set)]
    pub qd_mvar: f64,
    /// Shunt conductance (MW demanded at V=1.0 pu).
    #[pyo3(get, set)]
    pub gs_mw: f64,
    /// Shunt susceptance (MVAr injected at V=1.0 pu).
    #[pyo3(get, set)]
    pub bs_mvar: f64,
    /// Area number.
    #[pyo3(get, set)]
    pub area: u32,
    /// Zone number.
    #[pyo3(get, set)]
    pub zone: u32,
    /// Voltage magnitude initial value or flat-start value (pu).
    #[pyo3(get, set)]
    pub vm_pu: f64,
    /// Voltage angle initial value (degrees). Stored in radians internally.
    #[pyo3(get, set)]
    pub va_deg: f64,
    /// Base voltage (kV).
    #[pyo3(get, set)]
    pub base_kv: f64,
    /// Minimum voltage limit (pu).
    #[pyo3(get, set)]
    pub vmin_pu: f64,
    /// Maximum voltage limit (pu).
    #[pyo3(get, set)]
    pub vmax_pu: f64,
    /// Latitude in decimal degrees (WGS84). None if unknown.
    #[pyo3(get, set)]
    pub latitude: Option<f64>,
    /// Longitude in decimal degrees (WGS84). None if unknown.
    #[pyo3(get, set)]
    pub longitude: Option<f64>,
}

impl Bus {
    pub fn from_core_with_load(b: &CoreBus, pd_mw: f64, qd_mvar: f64) -> Self {
        Self {
            number: b.number,
            name: b.name.clone(),
            type_str: bus_type_str(b.bus_type),
            pd_mw,
            qd_mvar,
            gs_mw: b.shunt_conductance_mw,
            bs_mvar: b.shunt_susceptance_mvar,
            area: b.area,
            zone: b.zone,
            vm_pu: b.voltage_magnitude_pu,
            va_deg: b.voltage_angle_rad.to_degrees(),
            base_kv: b.base_kv,
            vmin_pu: b.voltage_min_pu,
            vmax_pu: b.voltage_max_pu,
            latitude: b.latitude,
            longitude: b.longitude,
        }
    }
}

#[pymethods]
impl Bus {
    #[new]
    #[pyo3(signature = (number, bus_type="PQ", base_kv=0.0, name="", pd_mw=0.0, qd_mvar=0.0, gs_mw=0.0, bs_mvar=0.0, area=1, zone=1, vm_pu=1.0, va_deg=0.0, vmin_pu=0.9, vmax_pu=1.1, latitude=None, longitude=None))]
    fn new(
        number: u32,
        bus_type: &str,
        base_kv: f64,
        name: &str,
        pd_mw: f64,
        qd_mvar: f64,
        gs_mw: f64,
        bs_mvar: f64,
        area: u32,
        zone: u32,
        vm_pu: f64,
        va_deg: f64,
        vmin_pu: f64,
        vmax_pu: f64,
        latitude: Option<f64>,
        longitude: Option<f64>,
    ) -> Self {
        Self {
            number,
            name: name.to_string(),
            type_str: bus_type.to_string(),
            pd_mw,
            qd_mvar,
            gs_mw,
            bs_mvar,
            area,
            zone,
            vm_pu,
            va_deg,
            base_kv,
            vmin_pu,
            vmax_pu,
            latitude,
            longitude,
        }
    }

    /// True if this is the Slack (reference) bus.
    #[getter]
    fn is_slack(&self) -> bool {
        self.type_str == "Slack"
    }
    /// True if this is a PV (generator) bus.
    #[getter]
    fn is_pv(&self) -> bool {
        self.type_str == "PV"
    }
    /// True if this is a PQ (load) bus.
    #[getter]
    fn is_pq(&self) -> bool {
        self.type_str == "PQ"
    }
    /// True if this bus is isolated (disconnected from network).
    #[getter]
    fn is_isolated(&self) -> bool {
        self.type_str == "Isolated"
    }
    /// True if latitude and longitude are both available.
    #[getter]
    fn has_coordinates(&self) -> bool {
        self.latitude.is_some() && self.longitude.is_some()
    }
    /// Voltage in kV (vm_pu * base_kv).
    #[getter]
    fn vm_kv(&self) -> f64 {
        self.vm_pu * self.base_kv
    }
    /// Apparent load |S_load| in MVA.
    #[getter]
    fn s_load_mva(&self) -> f64 {
        (self.pd_mw * self.pd_mw + self.qd_mvar * self.qd_mvar).sqrt()
    }
    /// True if voltage is outside [vmin_pu, vmax_pu].
    #[getter]
    fn is_voltage_violated(&self) -> bool {
        self.vm_pu < self.vmin_pu || self.vm_pu > self.vmax_pu
    }
    /// Voltage deviation from nominal (vm_pu - 1.0).
    #[getter]
    fn voltage_deviation_pu(&self) -> f64 {
        self.vm_pu - 1.0
    }

    fn __repr__(&self) -> String {
        format!(
            "<Bus {} '{}' {} {:.0}kV area={} Pd={:.1}MW Qd={:.1}MVAr V=[{:.3},{:.3}]pu>",
            self.number,
            self.name,
            self.type_str,
            self.base_kv,
            self.area,
            self.pd_mw,
            self.qd_mvar,
            self.vmin_pu,
            self.vmax_pu
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Branch
// ─────────────────────────────────────────────────────────────────────────────

/// A branch (transmission line or transformer) — all static model fields.
///
/// Obtain via `net.branches`, `net.branch(from_bus, to_bus)`.
#[pyclass(name = "Branch", skip_from_py_object)]
#[derive(Clone)]
pub struct Branch {
    /// From-bus external number.
    #[pyo3(get)]
    pub from_bus: u32,
    /// To-bus external number.
    #[pyo3(get)]
    pub to_bus: u32,
    /// Circuit identifier (1 for single-circuit lines).
    #[pyo3(get)]
    pub circuit: String,
    /// Series resistance (pu).
    #[pyo3(get, set)]
    pub r_pu: f64,
    /// Series reactance (pu).
    #[pyo3(get, set)]
    pub x_pu: f64,
    /// Total line charging susceptance (pu).
    #[pyo3(get, set)]
    pub b_pu: f64,
    /// Normal (long-term) MVA rating.
    #[pyo3(get, set)]
    pub rate_a_mva: f64,
    /// Short-term MVA rating.
    #[pyo3(get, set)]
    pub rate_b_mva: f64,
    /// Emergency MVA rating.
    #[pyo3(get, set)]
    pub rate_c_mva: f64,
    /// Off-nominal turns ratio (1.0 for lines, != 1.0 for transformers with voltage change).
    #[pyo3(get, set)]
    pub tap: f64,
    /// Phase shift angle (degrees). 0.0 for lines and fixed-tap transformers.
    #[pyo3(get, set)]
    pub shift_deg: f64,
    /// True if branch is in service.
    #[pyo3(get, set)]
    pub in_service: bool,
    /// Minimum phase angle difference (from-to) in degrees. None = unconstrained.
    #[pyo3(get, set)]
    pub angmin_deg: Option<f64>,
    /// Maximum phase angle difference (from-to) in degrees. None = unconstrained.
    #[pyo3(get, set)]
    pub angmax_deg: Option<f64>,
    /// Line charging conductance (pu). Normally 0 for overhead lines.
    #[pyo3(get, set)]
    pub g_pi: f64,
    /// Transformer magnetizing conductance (pu).
    #[pyo3(get, set)]
    pub g_mag: f64,
    /// Transformer magnetizing susceptance (pu).
    #[pyo3(get, set)]
    pub b_mag: f64,
    /// Zero-sequence winding connection type.
    #[pyo3(get, set)]
    pub transformer_connection: String,
    /// Whether delta-connected (blocks triplen harmonics).
    #[pyo3(get, set)]
    pub delta_connected: bool,
    /// Skin-effect resistance correction coefficient.
    #[pyo3(get, set)]
    pub skin_effect_alpha: f64,
    /// Tap control mode: "Fixed" or "Continuous" (for AC-OPF optimization).
    #[pyo3(get, set)]
    pub tap_mode: String,
    /// Minimum tap ratio (pu) for continuous tap optimization.
    #[pyo3(get, set)]
    pub tap_min: f64,
    /// Maximum tap ratio (pu) for continuous tap optimization.
    #[pyo3(get, set)]
    pub tap_max: f64,
    /// Phase-shift control mode: "Fixed" or "Continuous".
    #[pyo3(get, set)]
    pub phase_mode: String,
    /// Minimum phase shift (degrees) for continuous phase optimization.
    #[pyo3(get, set)]
    pub phase_min_deg: f64,
    /// Maximum phase shift (degrees) for continuous phase optimization.
    #[pyo3(get, set)]
    pub phase_max_deg: f64,
    /// Oil temperature limit (°C). None if not specified.
    #[pyo3(get, set)]
    pub oil_temp_limit_c: Option<f64>,
    /// Winding temperature limit (°C). None if not specified.
    #[pyo3(get, set)]
    pub winding_temp_limit_c: Option<f64>,
    /// Impedance limit (Ohms). None if not specified.
    #[pyo3(get, set)]
    pub impedance_limit_ohm: Option<f64>,
    /// Whether this branch has a transformer saturation curve attached.
    #[pyo3(get, set)]
    pub has_saturation: bool,
    /// Transformer core construction type (e.g. "ThreeLegCore"). None if not specified.
    #[pyo3(get, set)]
    pub core_type: Option<String>,
}

impl Branch {
    pub fn from_core(br: &CoreBranch) -> Self {
        use surge_network::network::{PhaseMode, TapMode, TransformerConnection};
        Self {
            from_bus: br.from_bus,
            to_bus: br.to_bus,
            circuit: br.circuit.clone(),
            r_pu: br.r,
            x_pu: br.x,
            b_pu: br.b,
            rate_a_mva: br.rating_a_mva,
            rate_b_mva: br.rating_b_mva,
            rate_c_mva: br.rating_c_mva,
            tap: br.tap,
            shift_deg: br.phase_shift_rad.to_degrees(),
            in_service: br.in_service,
            angmin_deg: br.angle_diff_min_rad.map(|a| a.to_degrees()),
            angmax_deg: br.angle_diff_max_rad.map(|a| a.to_degrees()),
            g_pi: br.g_pi,
            g_mag: br.g_mag,
            b_mag: br.b_mag,
            transformer_connection: match br
                .transformer_data
                .as_ref()
                .map(|t| t.transformer_connection)
                .unwrap_or_default()
            {
                TransformerConnection::WyeGWyeG => "WyeG-WyeG".to_string(),
                TransformerConnection::WyeGDelta => "WyeG-Delta".to_string(),
                TransformerConnection::DeltaWyeG => "Delta-WyeG".to_string(),
                TransformerConnection::DeltaDelta => "Delta-Delta".to_string(),
                TransformerConnection::WyeGWye => "WyeG-Wye".to_string(),
            },
            delta_connected: br.zero_seq.as_ref().is_some_and(|z| z.delta_connected),
            skin_effect_alpha: br
                .harmonic
                .as_ref()
                .map(|h| h.skin_effect_alpha)
                .unwrap_or(0.0),
            tap_mode: match br
                .opf_control
                .as_ref()
                .map(|c| c.tap_mode)
                .unwrap_or(TapMode::Fixed)
            {
                TapMode::Fixed => "Fixed".to_string(),
                TapMode::Continuous => "Continuous".to_string(),
            },
            tap_min: br.opf_control.as_ref().map(|c| c.tap_min).unwrap_or(0.9),
            tap_max: br.opf_control.as_ref().map(|c| c.tap_max).unwrap_or(1.1),
            phase_mode: match br
                .opf_control
                .as_ref()
                .map(|c| c.phase_mode)
                .unwrap_or(PhaseMode::Fixed)
            {
                PhaseMode::Fixed => "Fixed".to_string(),
                PhaseMode::Continuous => "Continuous".to_string(),
            },
            phase_min_deg: br
                .opf_control
                .as_ref()
                .map(|c| c.phase_min_rad)
                .unwrap_or((-30.0_f64).to_radians())
                .to_degrees(),
            phase_max_deg: br
                .opf_control
                .as_ref()
                .map(|c| c.phase_max_rad)
                .unwrap_or(30.0_f64.to_radians())
                .to_degrees(),
            oil_temp_limit_c: br
                .transformer_data
                .as_ref()
                .and_then(|t| t.oil_temp_limit_c),
            winding_temp_limit_c: br
                .transformer_data
                .as_ref()
                .and_then(|t| t.winding_temp_limit_c),
            impedance_limit_ohm: br
                .transformer_data
                .as_ref()
                .and_then(|t| t.impedance_limit_ohm),
            has_saturation: br
                .harmonic
                .as_ref()
                .and_then(|h| h.saturation.clone())
                .is_some(),
            core_type: br
                .harmonic
                .as_ref()
                .and_then(|h| h.core_type)
                .as_ref()
                .map(|ct| ct.to_string()),
        }
    }

    /// Effective tap ratio, normalizing MATPOWER's tap=0 convention to 1.0.
    #[inline]
    pub fn effective_tap(&self) -> f64 {
        if self.tap.abs() < 1e-10 {
            1.0
        } else {
            self.tap
        }
    }
}

#[pymethods]
impl Branch {
    #[new]
    #[pyo3(signature = (from_bus, to_bus, circuit="1", r_pu=0.0, x_pu=0.0, b_pu=0.0, rate_a_mva=0.0, rate_b_mva=0.0, rate_c_mva=0.0, tap=1.0, shift_deg=0.0, in_service=true, angmin_deg=None, angmax_deg=None, g_pi=0.0, g_mag=0.0, b_mag=0.0, transformer_connection="WyeG-WyeG", delta_connected=false, skin_effect_alpha=0.0, tap_mode="Fixed", tap_min=0.9, tap_max=1.1, phase_mode="Fixed", phase_min_deg=0.0, phase_max_deg=0.0, oil_temp_limit_c=None, winding_temp_limit_c=None, impedance_limit_ohm=None, has_saturation=false, core_type=None))]
    fn new(
        from_bus: u32,
        to_bus: u32,
        circuit: &str,
        r_pu: f64,
        x_pu: f64,
        b_pu: f64,
        rate_a_mva: f64,
        rate_b_mva: f64,
        rate_c_mva: f64,
        tap: f64,
        shift_deg: f64,
        in_service: bool,
        angmin_deg: Option<f64>,
        angmax_deg: Option<f64>,
        g_pi: f64,
        g_mag: f64,
        b_mag: f64,
        transformer_connection: &str,
        delta_connected: bool,
        skin_effect_alpha: f64,
        tap_mode: &str,
        tap_min: f64,
        tap_max: f64,
        phase_mode: &str,
        phase_min_deg: f64,
        phase_max_deg: f64,
        oil_temp_limit_c: Option<f64>,
        winding_temp_limit_c: Option<f64>,
        impedance_limit_ohm: Option<f64>,
        has_saturation: bool,
        core_type: Option<String>,
    ) -> Self {
        Self {
            from_bus,
            to_bus,
            circuit: circuit.to_string(),
            r_pu,
            x_pu,
            b_pu,
            rate_a_mva,
            rate_b_mva,
            rate_c_mva,
            tap,
            shift_deg,
            in_service,
            angmin_deg,
            angmax_deg,
            g_pi,
            g_mag,
            b_mag,
            transformer_connection: transformer_connection.to_string(),
            delta_connected,
            skin_effect_alpha,
            tap_mode: tap_mode.to_string(),
            tap_min,
            tap_max,
            phase_mode: phase_mode.to_string(),
            phase_min_deg,
            phase_max_deg,
            oil_temp_limit_c,
            winding_temp_limit_c,
            impedance_limit_ohm,
            has_saturation,
            core_type,
        }
    }

    /// True if this branch is a transformer (tap != 1.0, phase shift != 0.0, or has magnetizing admittance).
    #[getter]
    fn is_transformer(&self) -> bool {
        let tap = self.effective_tap();
        (tap - 1.0).abs() > 1e-6
            || self.shift_deg.abs() > 1e-6
            || self.g_mag.abs() > 1e-12
            || self.b_mag.abs() > 1e-12
            || self.transformer_connection != "WyeG-WyeG"
    }

    /// DC series susceptance (1 / (x * tap)), corrected for off-nominal tap.
    /// Used in DC power flow and PTDF/LODF computations.
    #[getter]
    fn b_dc_pu(&self) -> f64 {
        let tap = self.effective_tap();
        let denom = self.x_pu * tap;
        if denom.abs() < 1e-20 {
            0.0
        } else {
            1.0 / denom
        }
    }

    /// X/R ratio (f64::INFINITY if r_pu == 0).
    #[getter]
    fn x_r_ratio(&self) -> f64 {
        if self.r_pu.abs() < 1e-20 {
            f64::INFINITY
        } else {
            self.x_pu / self.r_pu
        }
    }
    /// Transformer magnetizing impedance (pu) as (real, imag). INFINITY if g_mag == 0 and b_mag == 0.
    #[getter]
    fn z_mag_pu(&self) -> (f64, f64) {
        if self.g_mag.abs() < 1e-20 && self.b_mag.abs() < 1e-20 {
            (f64::INFINITY, f64::INFINITY)
        } else {
            let mag_sq = self.g_mag * self.g_mag + self.b_mag * self.b_mag;
            (self.g_mag / mag_sq, -self.b_mag / mag_sq)
        }
    }
    /// Series admittance as (g_pu, b_pu) tuple.
    #[getter]
    fn y_series_pu(&self) -> (f64, f64) {
        let z_sq = self.r_pu * self.r_pu + self.x_pu * self.x_pu;
        if z_sq < 1e-40 {
            (1e6, 0.0)
        } else {
            (self.r_pu / z_sq, -self.x_pu / z_sq)
        }
    }
    /// Series impedance as (r_pu, x_pu) tuple.
    #[getter]
    fn z_pu(&self) -> (f64, f64) {
        (self.r_pu, self.x_pu)
    }

    fn __repr__(&self) -> String {
        let kind = if self.is_transformer() {
            "Transformer"
        } else {
            "Line"
        };
        format!(
            "<Branch {}→{} ckt={} {} r={:.5} x={:.5} rate_a={:.0}MVA{}>",
            self.from_bus,
            self.to_bus,
            self.circuit,
            kind,
            self.r_pu,
            self.x_pu,
            self.rate_a_mva,
            if self.in_service {
                ""
            } else {
                " OUT-OF-SERVICE"
            }
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Generator
// ─────────────────────────────────────────────────────────────────────────────

/// Storage parameters for a generator-backed storage resource.
#[pyclass(name = "StorageParams", from_py_object)]
#[derive(Clone)]
pub struct StorageParams {
    #[pyo3(get, set)]
    pub charge_efficiency: f64,
    #[pyo3(get, set)]
    pub discharge_efficiency: f64,
    #[pyo3(get, set)]
    pub energy_capacity_mwh: f64,
    #[pyo3(get, set)]
    pub soc_initial_mwh: f64,
    #[pyo3(get, set)]
    pub soc_min_mwh: f64,
    #[pyo3(get, set)]
    pub soc_max_mwh: f64,
    #[pyo3(get, set)]
    pub variable_cost_per_mwh: f64,
    #[pyo3(get, set)]
    pub degradation_cost_per_mwh: f64,
    #[pyo3(get, set)]
    pub dispatch_mode: String,
    #[pyo3(get, set)]
    pub self_schedule_mw: f64,
    #[pyo3(get, set)]
    pub discharge_offer: Option<Vec<(f64, f64)>>,
    #[pyo3(get, set)]
    pub charge_bid: Option<Vec<(f64, f64)>>,
    #[pyo3(get, set)]
    pub max_c_rate_charge: Option<f64>,
    #[pyo3(get, set)]
    pub max_c_rate_discharge: Option<f64>,
    #[pyo3(get, set)]
    pub chemistry: Option<String>,
    #[pyo3(get, set)]
    pub discharge_foldback_soc_mwh: Option<f64>,
    #[pyo3(get, set)]
    pub charge_foldback_soc_mwh: Option<f64>,
}

impl StorageParams {
    pub fn from_core(storage: &CoreStorageParams) -> Self {
        Self {
            charge_efficiency: storage.charge_efficiency,
            discharge_efficiency: storage.discharge_efficiency,
            energy_capacity_mwh: storage.energy_capacity_mwh,
            soc_initial_mwh: storage.soc_initial_mwh,
            soc_min_mwh: storage.soc_min_mwh,
            soc_max_mwh: storage.soc_max_mwh,
            variable_cost_per_mwh: storage.variable_cost_per_mwh,
            degradation_cost_per_mwh: storage.degradation_cost_per_mwh,
            dispatch_mode: storage_dispatch_mode_str(storage.dispatch_mode).to_string(),
            self_schedule_mw: storage.self_schedule_mw,
            discharge_offer: storage.discharge_offer.clone(),
            charge_bid: storage.charge_bid.clone(),
            max_c_rate_charge: storage.max_c_rate_charge,
            max_c_rate_discharge: storage.max_c_rate_discharge,
            chemistry: storage.chemistry.clone(),
            discharge_foldback_soc_mwh: storage.discharge_foldback_soc_mwh,
            charge_foldback_soc_mwh: storage.charge_foldback_soc_mwh,
        }
    }

    pub fn to_core(&self) -> PyResult<CoreStorageParams> {
        Ok(CoreStorageParams {
            charge_efficiency: self.charge_efficiency,
            discharge_efficiency: self.discharge_efficiency,
            energy_capacity_mwh: self.energy_capacity_mwh,
            soc_initial_mwh: self.soc_initial_mwh,
            soc_min_mwh: self.soc_min_mwh,
            soc_max_mwh: self.soc_max_mwh,
            variable_cost_per_mwh: self.variable_cost_per_mwh,
            degradation_cost_per_mwh: self.degradation_cost_per_mwh,
            dispatch_mode: parse_storage_dispatch_mode(&self.dispatch_mode)?,
            self_schedule_mw: self.self_schedule_mw,
            discharge_offer: self.discharge_offer.clone(),
            charge_bid: self.charge_bid.clone(),
            max_c_rate_charge: self.max_c_rate_charge,
            max_c_rate_discharge: self.max_c_rate_discharge,
            chemistry: self.chemistry.clone(),
            discharge_foldback_soc_mwh: self.discharge_foldback_soc_mwh,
            charge_foldback_soc_mwh: self.charge_foldback_soc_mwh,
        })
    }
}

#[pymethods]
impl StorageParams {
    #[new]
    #[pyo3(signature = (energy_capacity_mwh, charge_efficiency=None, discharge_efficiency=None, efficiency=None, soc_initial_mwh=None, soc_min_mwh=0.0, soc_max_mwh=None, variable_cost_per_mwh=0.0, degradation_cost_per_mwh=0.0, dispatch_mode="cost_minimization".to_string(), self_schedule_mw=0.0, discharge_offer=None, charge_bid=None, max_c_rate_charge=None, max_c_rate_discharge=None, chemistry=None, discharge_foldback_soc_mwh=None, charge_foldback_soc_mwh=None))]
    fn new(
        energy_capacity_mwh: f64,
        charge_efficiency: Option<f64>,
        discharge_efficiency: Option<f64>,
        efficiency: Option<f64>,
        soc_initial_mwh: Option<f64>,
        soc_min_mwh: f64,
        soc_max_mwh: Option<f64>,
        variable_cost_per_mwh: f64,
        degradation_cost_per_mwh: f64,
        dispatch_mode: String,
        self_schedule_mw: f64,
        discharge_offer: Option<Vec<(f64, f64)>>,
        charge_bid: Option<Vec<(f64, f64)>>,
        max_c_rate_charge: Option<f64>,
        max_c_rate_discharge: Option<f64>,
        chemistry: Option<String>,
        discharge_foldback_soc_mwh: Option<f64>,
        charge_foldback_soc_mwh: Option<f64>,
    ) -> PyResult<Self> {
        // Efficiency resolution:
        //   * If charge_efficiency / discharge_efficiency are provided, use
        //     them directly.
        //   * Else if a legacy `efficiency` (round-trip) scalar is given,
        //     split it symmetrically as sqrt(eta) per leg.
        //   * Else default to 0.90 charge / 0.98 discharge.
        let (eta_ch, eta_dis) = match (charge_efficiency, discharge_efficiency, efficiency) {
            (Some(c), Some(d), _) => (c, d),
            (Some(c), None, _) => (c, 0.98),
            (None, Some(d), _) => (0.90, d),
            (None, None, Some(rt)) => {
                let leg = rt.max(0.0).sqrt();
                (leg, leg)
            }
            (None, None, None) => (0.90, 0.98),
        };
        let storage = Self {
            charge_efficiency: eta_ch,
            discharge_efficiency: eta_dis,
            energy_capacity_mwh,
            soc_initial_mwh: soc_initial_mwh.unwrap_or(0.5 * energy_capacity_mwh),
            soc_min_mwh,
            soc_max_mwh: soc_max_mwh.unwrap_or(energy_capacity_mwh),
            variable_cost_per_mwh,
            degradation_cost_per_mwh,
            dispatch_mode,
            self_schedule_mw,
            discharge_offer,
            charge_bid,
            max_c_rate_charge,
            max_c_rate_discharge,
            chemistry,
            discharge_foldback_soc_mwh,
            charge_foldback_soc_mwh,
        };
        let _ = storage.to_core()?;
        Ok(storage)
    }

    /// Round-trip efficiency implied by the charge/discharge pair. Read-only
    /// convenience for inspection and logging.
    #[getter]
    fn round_trip_efficiency(&self) -> f64 {
        self.charge_efficiency * self.discharge_efficiency
    }

    fn __repr__(&self) -> String {
        format!(
            "<StorageParams E={:.1}MWh η={:.2}/{:.2} SoC=[{:.1},{:.1},{:.1}] mode={}>",
            self.energy_capacity_mwh,
            self.charge_efficiency,
            self.discharge_efficiency,
            self.soc_min_mwh,
            self.soc_initial_mwh,
            self.soc_max_mwh,
            self.dispatch_mode
        )
    }
}

/// A generator connected to a bus — all static model fields.
///
/// Obtain via `net.generators` or `net.generator(id)`.
#[pyclass(name = "Generator", skip_from_py_object)]
#[derive(Clone)]
pub struct Generator {
    /// Canonical generator identifier.
    #[pyo3(get, set)]
    pub id: String,
    /// Bus number where the generator is connected.
    #[pyo3(get)]
    pub bus: u32,
    /// PSS/E machine ID (defaults to "1").
    #[pyo3(get)]
    pub machine_id: String,
    /// Scheduled active power output (MW). Model value, not solved dispatch.
    #[pyo3(get, set)]
    pub p_mw: f64,
    /// Scheduled reactive power output (MVAr). Model value, not solved.
    #[pyo3(get, set)]
    pub q_mvar: f64,
    /// Maximum active power (MW).
    #[pyo3(get, set)]
    pub pmax_mw: f64,
    /// Minimum active power (MW).
    #[pyo3(get, set)]
    pub pmin_mw: f64,
    /// Maximum reactive power (MVAr).
    #[pyo3(get, set)]
    pub qmax_mvar: f64,
    /// Minimum reactive power (MVAr).
    #[pyo3(get, set)]
    pub qmin_mvar: f64,
    /// Voltage setpoint (pu).
    #[pyo3(get, set)]
    pub vs_pu: f64,
    /// Machine base MVA.
    #[pyo3(get, set)]
    pub mbase_mva: f64,
    /// True if generator is in service.
    #[pyo3(get, set)]
    pub in_service: bool,
    /// Fuel type (e.g. "gas", "coal", "nuclear", "wind", "solar"). None if unspecified.
    #[pyo3(get, set)]
    pub fuel_type: Option<String>,
    /// Heat rate (BTU/MWh). None if unspecified.
    #[pyo3(get, set)]
    pub heat_rate_btu_mwh: Option<f64>,
    /// CO2 emission rate (tonnes/MWh). Zero for zero-emission resources.
    #[pyo3(get, set)]
    pub co2_rate_t_per_mwh: f64,
    /// NOx emission rate (tonnes/MWh).
    #[pyo3(get, set)]
    pub nox_rate_t_per_mwh: f64,
    /// SO2 emission rate (tonnes/MWh).
    #[pyo3(get, set)]
    pub so2_rate_t_per_mwh: f64,
    /// PM2.5 emission rate (tonnes/MWh).
    #[pyo3(get, set)]
    pub pm25_rate_t_per_mwh: f64,
    /// Forced outage rate [0, 1]. None if unspecified.
    #[pyo3(get, set)]
    pub forced_outage_rate: Option<f64>,
    // Ramp curves — (MW operating-point, MW/min) segments
    /// Normal ramp-up curve. Empty = unlimited.
    #[pyo3(get, set)]
    pub ramp_up_curve: Vec<(f64, f64)>,
    /// Normal ramp-down curve. Empty = unlimited.
    #[pyo3(get, set)]
    pub ramp_down_curve: Vec<(f64, f64)>,
    /// Regulation (AGC) ramp-up curve. Empty = falls back to ramp_up_curve.
    #[pyo3(get, set)]
    pub reg_ramp_up_curve: Vec<(f64, f64)>,
    /// Upward ramp rate (MW/min) from first segment. None if no curve.
    #[pyo3(get)]
    pub ramp_up_mw_per_min: Option<f64>,
    /// Downward ramp rate (MW/min) from first segment. Falls back to ramp_up if no down curve.
    #[pyo3(get)]
    pub ramp_dn_mw_per_min: Option<f64>,
    /// AGC ramp rate (MW/min) from reg curve. Falls back to ramp_up if no reg curve.
    #[pyo3(get)]
    pub ramp_agc_mw_per_min: Option<f64>,
    // Commitment
    /// Commitment status: "Market", "SelfCommitted", "MustRun", "Unavailable", "EmergencyOnly".
    #[pyo3(get, set)]
    pub commitment_status: String,
    /// Minimum up time (hours). None if unspecified.
    #[pyo3(get, set)]
    pub min_up_time_hr: Option<f64>,
    /// Minimum down time (hours). None if unspecified.
    #[pyo3(get, set)]
    pub min_down_time_hr: Option<f64>,
    /// Maximum up time (hours). None if unspecified.
    #[pyo3(get, set)]
    pub max_up_time_hr: Option<f64>,
    /// Minimum soak time at pmin after sync (hours). None if unspecified.
    #[pyo3(get, set)]
    pub min_run_at_pmin_hr: Option<f64>,
    /// Maximum startup events in a rolling 24-hour window. None if unspecified.
    #[pyo3(get, set)]
    pub max_starts_per_day: Option<u32>,
    /// Maximum startup events in a rolling 168-hour window. None if unspecified.
    #[pyo3(get, set)]
    pub max_starts_per_week: Option<u32>,
    /// Maximum energy in a rolling 24-hour window (MWh). None if unspecified.
    #[pyo3(get, set)]
    pub max_energy_mwh_per_day: Option<f64>,
    /// Maximum startup output ramp (MW/min). None if unspecified.
    #[pyo3(get, set)]
    pub startup_ramp_mw_per_min: Option<f64>,
    /// Maximum shutdown output ramp (MW/min). None if unspecified.
    #[pyo3(get, set)]
    pub shutdown_ramp_mw_per_min: Option<f64>,
    /// Startup cost tiers: [(max_offline_hours, cost_$, sync_time_min), ...].
    /// Sourced from energy_offer.submitted.startup_tiers if present,
    /// falling back to legacy startup_cost_tiers (with sync_time=0).
    #[pyo3(get, set)]
    pub startup_cost_tiers: Vec<(f64, f64, f64)>,
    /// True if this is a quick-start unit (≤10 min to full output).
    #[pyo3(get, set)]
    pub quick_start: bool,
    /// Optional storage overlay for generator-backed storage resources.
    #[pyo3(get, set)]
    pub storage: Option<StorageParams>,
    // Reserve offers: list of (product_id, capacity_mw, cost_per_mwh)
    #[pyo3(get, set)]
    pub reserve_offers: Vec<(String, f64, f64)>,
    // Reserve qualification flags: {product_id: qualified}
    #[pyo3(get, set)]
    pub qualifications: std::collections::HashMap<String, bool>,
    // Reactive capability curve (MATPOWER-style 2-point)
    #[pyo3(get, set)]
    pub pc1_mw: Option<f64>,
    #[pyo3(get, set)]
    pub pc2_mw: Option<f64>,
    #[pyo3(get, set)]
    pub qc1min_mvar: Option<f64>,
    #[pyo3(get, set)]
    pub qc1max_mvar: Option<f64>,
    #[pyo3(get, set)]
    pub qc2min_mvar: Option<f64>,
    #[pyo3(get, set)]
    pub qc2max_mvar: Option<f64>,
    /// Extended P-Q capability curve: [(p_pu, qmax_pu, qmin_pu), ...].
    #[pyo3(get, set)]
    pub pq_curve: Vec<(f64, f64, f64)>,
    // Dynamics
    /// Inertia constant H (seconds = MVA·s/MVA). None if unspecified.
    #[pyo3(get, set)]
    pub h_inertia_s: Option<f64>,
    /// Machine leakage reactance Xd' (pu, machine base). None if unspecified.
    #[pyo3(get, set)]
    pub xs_pu: Option<f64>,
    /// AGC participation factor. None if unspecified.
    #[pyo3(get, set)]
    pub apf: Option<f64>,
    /// Negative-sequence subtransient reactance X2 (pu, machine base). None if unspecified.
    #[pyo3(get, set)]
    pub x2_pu: Option<f64>,
    /// Neutral grounding impedance (real_pu, imag_pu). None if solidly grounded.
    #[pyo3(get, set)]
    pub zn_pu: Option<(f64, f64)>,
    // Cost curve (flattened from CostCurve enum)
    /// Cost model: "polynomial", "piecewise_linear", or None if no cost data.
    #[pyo3(get, set)]
    pub cost_model: Option<String>,
    /// Startup cost ($). 0 if no cost data.
    #[pyo3(get, set)]
    pub cost_startup: f64,
    /// Shutdown cost ($). 0 if no cost data.
    #[pyo3(get, set)]
    pub cost_shutdown: f64,
    /// Polynomial coefficients [c_{n-1}, ..., c_1, c_0] or empty for piecewise.
    #[pyo3(get, set)]
    pub cost_coefficients: Vec<f64>,
    /// Piecewise-linear breakpoints: MW values (empty for polynomial).
    #[pyo3(get, set)]
    pub cost_breakpoints_mw: Vec<f64>,
    /// Piecewise-linear breakpoints: $/hr values (empty for polynomial).
    #[pyo3(get, set)]
    pub cost_breakpoints_usd: Vec<f64>,
}

impl Generator {
    pub fn from_core(g: &CoreGenerator) -> Self {
        let (
            cost_model,
            cost_startup,
            cost_shutdown,
            cost_coefficients,
            cost_breakpoints_mw,
            cost_breakpoints_usd,
        ) = flatten_cost_curve(&g.cost);
        Self {
            id: g.id.clone(),
            bus: g.bus,
            machine_id: g.machine_id.clone().unwrap_or_else(|| "1".to_string()),
            p_mw: g.p,
            q_mvar: g.q,
            pmax_mw: g.pmax,
            pmin_mw: g.pmin,
            qmax_mvar: g.qmax,
            qmin_mvar: g.qmin,
            vs_pu: g.voltage_setpoint_pu,
            mbase_mva: g.machine_base_mva,
            in_service: g.in_service,
            fuel_type: g.fuel.as_ref().and_then(|f| f.fuel_type.clone()),
            heat_rate_btu_mwh: g.fuel.as_ref().and_then(|f| f.heat_rate_btu_mwh),
            co2_rate_t_per_mwh: g.fuel.as_ref().map(|f| f.emission_rates.co2).unwrap_or(0.0),
            nox_rate_t_per_mwh: g.fuel.as_ref().map(|f| f.emission_rates.nox).unwrap_or(0.0),
            so2_rate_t_per_mwh: g.fuel.as_ref().map(|f| f.emission_rates.so2).unwrap_or(0.0),
            pm25_rate_t_per_mwh: g
                .fuel
                .as_ref()
                .map(|f| f.emission_rates.pm25)
                .unwrap_or(0.0),
            forced_outage_rate: g.forced_outage_rate,
            ramp_up_curve: g
                .ramping
                .as_ref()
                .map(|r| r.ramp_up_curve.clone())
                .unwrap_or_default(),
            ramp_down_curve: g
                .ramping
                .as_ref()
                .map(|r| r.ramp_down_curve.clone())
                .unwrap_or_default(),
            reg_ramp_up_curve: g
                .ramping
                .as_ref()
                .map(|r| r.reg_ramp_up_curve.clone())
                .unwrap_or_default(),
            ramp_up_mw_per_min: g.ramp_up_mw_per_min(),
            ramp_dn_mw_per_min: g.ramp_down_mw_per_min(),
            ramp_agc_mw_per_min: g.ramp_agc_mw_per_min(),
            commitment_status: commitment_status_str(
                g.commitment
                    .as_ref()
                    .map(|c| c.status)
                    .unwrap_or(CommitmentStatus::Market),
            ),
            min_up_time_hr: g.commitment.as_ref().and_then(|c| c.min_up_time_hr),
            min_down_time_hr: g.commitment.as_ref().and_then(|c| c.min_down_time_hr),
            max_up_time_hr: g.commitment.as_ref().and_then(|c| c.max_up_time_hr),
            min_run_at_pmin_hr: g.commitment.as_ref().and_then(|c| c.min_run_at_pmin_hr),
            max_starts_per_day: g.commitment.as_ref().and_then(|c| c.max_starts_per_day),
            max_starts_per_week: g.commitment.as_ref().and_then(|c| c.max_starts_per_week),
            max_energy_mwh_per_day: g.commitment.as_ref().and_then(|c| c.max_energy_mwh_per_day),
            startup_ramp_mw_per_min: g
                .commitment
                .as_ref()
                .and_then(|c| c.startup_ramp_mw_per_min),
            shutdown_ramp_mw_per_min: g
                .commitment
                .as_ref()
                .and_then(|c| c.shutdown_ramp_mw_per_min),
            startup_cost_tiers: g
                .market
                .as_ref()
                .and_then(|m| m.energy_offer.as_ref())
                .map(|eo| {
                    eo.submitted
                        .startup_tiers
                        .iter()
                        .map(|t| (t.max_offline_hours, t.cost, t.sync_time_min))
                        .collect()
                })
                .unwrap_or_default(),
            quick_start: g.quick_start,
            storage: g.storage.as_ref().map(StorageParams::from_core),
            reserve_offers: g
                .market
                .as_ref()
                .map(|m| {
                    m.reserve_offers
                        .iter()
                        .map(|o| (o.product_id.clone(), o.capacity_mw, o.cost_per_mwh))
                        .collect()
                })
                .unwrap_or_default(),
            qualifications: g
                .market
                .as_ref()
                .map(|m| m.qualifications.clone())
                .unwrap_or_default(),
            pc1_mw: g.reactive_capability.as_ref().and_then(|r| r.pc1),
            pc2_mw: g.reactive_capability.as_ref().and_then(|r| r.pc2),
            qc1min_mvar: g.reactive_capability.as_ref().and_then(|r| r.qc1min),
            qc1max_mvar: g.reactive_capability.as_ref().and_then(|r| r.qc1max),
            qc2min_mvar: g.reactive_capability.as_ref().and_then(|r| r.qc2min),
            qc2max_mvar: g.reactive_capability.as_ref().and_then(|r| r.qc2max),
            pq_curve: g
                .reactive_capability
                .as_ref()
                .map(|r| r.pq_curve.clone())
                .unwrap_or_default(),
            h_inertia_s: g.h_inertia_s,
            xs_pu: g.fault_data.as_ref().and_then(|f| f.xs),
            apf: g.agc_participation_factor,
            x2_pu: g.fault_data.as_ref().and_then(|f| f.x2_pu),
            zn_pu: g
                .fault_data
                .as_ref()
                .and_then(|f| f.zn)
                .map(|z| (z.re, z.im)),
            cost_model,
            cost_startup,
            cost_shutdown,
            cost_coefficients,
            cost_breakpoints_mw,
            cost_breakpoints_usd,
        }
    }
}

#[pymethods]
impl Generator {
    #[new]
    #[pyo3(signature = (bus, machine_id="1", id=None))]
    fn new(bus: u32, machine_id: &str, id: Option<String>) -> Self {
        Self {
            id: id.unwrap_or_default(),
            bus,
            machine_id: machine_id.to_string(),
            p_mw: 0.0,
            q_mvar: 0.0,
            pmax_mw: 0.0,
            pmin_mw: 0.0,
            qmax_mvar: 9999.0,
            qmin_mvar: -9999.0,
            vs_pu: 1.0,
            mbase_mva: 100.0,
            in_service: true,
            fuel_type: None,
            heat_rate_btu_mwh: None,
            co2_rate_t_per_mwh: 0.0,
            nox_rate_t_per_mwh: 0.0,
            so2_rate_t_per_mwh: 0.0,
            pm25_rate_t_per_mwh: 0.0,
            forced_outage_rate: None,
            ramp_up_curve: Vec::new(),
            ramp_down_curve: Vec::new(),
            reg_ramp_up_curve: Vec::new(),
            ramp_up_mw_per_min: None,
            ramp_dn_mw_per_min: None,
            ramp_agc_mw_per_min: None,
            commitment_status: "Market".to_string(),
            min_up_time_hr: None,
            min_down_time_hr: None,
            max_up_time_hr: None,
            min_run_at_pmin_hr: None,
            max_starts_per_day: None,
            max_starts_per_week: None,
            max_energy_mwh_per_day: None,
            startup_ramp_mw_per_min: None,
            shutdown_ramp_mw_per_min: None,
            startup_cost_tiers: Vec::new(),
            quick_start: false,
            storage: None,
            reserve_offers: Vec::new(),
            qualifications: std::collections::HashMap::new(),
            pc1_mw: None,
            pc2_mw: None,
            qc1min_mvar: None,
            qc1max_mvar: None,
            qc2min_mvar: None,
            qc2max_mvar: None,
            pq_curve: Vec::new(),
            h_inertia_s: None,
            xs_pu: None,
            apf: None,
            x2_pu: None,
            zn_pu: None,
            cost_model: None,
            cost_startup: 0.0,
            cost_shutdown: 0.0,
            cost_coefficients: Vec::new(),
            cost_breakpoints_mw: Vec::new(),
            cost_breakpoints_usd: Vec::new(),
        }
    }

    /// Canonical resource identifier used in dispatch requests.
    ///
    /// Semantic alias for ``id``. A freshly-added generator via
    /// :meth:`Network.add_generator` always has a canonical id of the
    /// form ``gen_{bus}_{ordinal}``; loaders that deserialise from
    /// MATPOWER / PSS/E / GO C3 canonicalise at load time. Use this
    /// when you're writing a ``resource_id`` into an offer schedule,
    /// commitment initial condition, or any other per-resource entry
    /// of a ``DispatchRequest`` — the name signals intent.
    #[getter]
    fn resource_id(&self) -> &str {
        &self.id
    }

    /// True if this generator has a cost curve.
    #[getter]
    fn has_cost(&self) -> bool {
        self.cost_model.is_some()
    }

    /// Constant term c₀ of a polynomial cost curve ($/hr at P=0).
    ///
    /// Returns `None` if the cost model is not polynomial.
    /// For `f(P) = c2·P² + c1·P + c0`, this is the fixed/no-load cost.
    #[getter]
    fn cost_c0(&self) -> Option<f64> {
        if self.cost_model.as_deref() != Some("polynomial") {
            return None;
        }
        self.cost_coefficients.last().copied()
    }
    /// Linear coefficient c₁ of a polynomial cost curve ($/MWh at P=0).
    ///
    /// Returns `None` if the cost model is not polynomial or has fewer than 2 coefficients.
    #[getter]
    fn cost_c1(&self) -> Option<f64> {
        if self.cost_model.as_deref() != Some("polynomial") {
            return None;
        }
        let n = self.cost_coefficients.len();
        if n >= 2 {
            Some(self.cost_coefficients[n - 2])
        } else {
            None
        }
    }
    /// Quadratic coefficient c₂ of a polynomial cost curve ($/MW²·hr).
    ///
    /// Returns `None` if the cost model is not polynomial or has fewer than 3 coefficients.
    /// For a linear cost curve, this will be `None`.
    #[getter]
    fn cost_c2(&self) -> Option<f64> {
        if self.cost_model.as_deref() != Some("polynomial") {
            return None;
        }
        let n = self.cost_coefficients.len();
        if n >= 3 {
            Some(self.cost_coefficients[n - 3])
        } else {
            None
        }
    }

    /// True if this generator has a P-Q capability curve.
    #[getter]
    fn has_reactive_capability_curve(&self) -> bool {
        !self.pq_curve.is_empty()
    }
    /// True if commitment_status is "MustRun".
    #[getter]
    fn must_run(&self) -> bool {
        self.commitment_status == "MustRun"
    }
    /// True if this generator offers any reserve product.
    #[getter]
    fn has_ancillary_services(&self) -> bool {
        !self.reserve_offers.is_empty()
    }

    /// Total cost ($/hr) at a given dispatch level.
    ///
    /// Returns 0.0 if no cost curve is available.
    fn cost_at(&self, p_mw: f64) -> f64 {
        match self.cost_model.as_deref() {
            Some("polynomial") => eval_polynomial(&self.cost_coefficients, p_mw),
            Some("piecewise_linear") => {
                let pts: Vec<(f64, f64)> = self
                    .cost_breakpoints_mw
                    .iter()
                    .zip(self.cost_breakpoints_usd.iter())
                    .map(|(&m, &u)| (m, u))
                    .collect();
                eval_piecewise(&pts, p_mw)
            }
            _ => 0.0,
        }
    }

    /// Marginal cost ($/MWh) at a given dispatch level.
    ///
    /// For polynomial curves: derivative of cost curve.
    /// For piecewise-linear: slope of active segment.
    /// Returns 0.0 if no cost curve is available.
    fn marginal_cost_at(&self, p_mw: f64) -> f64 {
        match self.cost_model.as_deref() {
            Some("polynomial") => marginal_polynomial(&self.cost_coefficients, p_mw),
            Some("piecewise_linear") => {
                let pts: Vec<(f64, f64)> = self
                    .cost_breakpoints_mw
                    .iter()
                    .zip(self.cost_breakpoints_usd.iter())
                    .map(|(&m, &u)| (m, u))
                    .collect();
                marginal_piecewise(&pts, p_mw)
            }
            _ => 0.0,
        }
    }

    /// Active power capacity (pmax - pmin).
    #[getter]
    fn capacity_mw(&self) -> f64 {
        self.pmax_mw - self.pmin_mw
    }
    /// Active power headroom (pmax - p_mw).
    #[getter]
    fn headroom_mw(&self) -> f64 {
        self.pmax_mw - self.p_mw
    }
    /// Reactive power range (qmax - qmin).
    #[getter]
    fn reactive_range_mvar(&self) -> f64 {
        self.qmax_mvar - self.qmin_mvar
    }
    /// Power factor at current dispatch. NaN if both pg and qg are 0.
    #[getter]
    fn power_factor(&self) -> f64 {
        let s = (self.p_mw * self.p_mw + self.q_mvar * self.q_mvar).sqrt();
        if s < 1e-10 { f64::NAN } else { self.p_mw / s }
    }

    fn __repr__(&self) -> String {
        let fuel = self.fuel_type.as_deref().unwrap_or("?");
        format!(
            "<Generator bus={} id='{}' {} Pg={:.1}/{:.1}MW Qg={:.1}MVAr{}>",
            self.bus,
            self.machine_id,
            fuel,
            self.p_mw,
            self.pmax_mw,
            self.q_mvar,
            if self.in_service {
                ""
            } else {
                " OUT-OF-SERVICE"
            }
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Load
// ─────────────────────────────────────────────────────────────────────────────

/// A load connected to a bus.
///
/// Obtain via `net.loads`.
#[pyclass(name = "Load", skip_from_py_object)]
#[derive(Clone)]
pub struct Load {
    /// Bus number.
    #[pyo3(get)]
    pub bus: u32,
    /// Load identifier.
    #[pyo3(get)]
    pub id: String,
    /// Real power demand (MW).
    #[pyo3(get, set)]
    pub pd_mw: f64,
    /// Reactive power demand (MVAr).
    #[pyo3(get, set)]
    pub qd_mvar: f64,
    /// True if load is in service.
    #[pyo3(get, set)]
    pub in_service: bool,
    /// True if load conforms to system-wide scaling forecasts.
    #[pyo3(get, set)]
    pub conforming: bool,
}

impl Load {
    pub fn from_core(l: &CoreLoad) -> Self {
        Self {
            bus: l.bus,
            id: l.id.clone(),
            pd_mw: l.active_power_demand_mw,
            qd_mvar: l.reactive_power_demand_mvar,
            in_service: l.in_service,
            conforming: l.conforming,
        }
    }
}

#[pymethods]
impl Load {
    #[new]
    #[pyo3(signature = (bus, id="1", pd_mw=0.0, qd_mvar=0.0, in_service=true, conforming=true))]
    fn new(
        bus: u32,
        id: &str,
        pd_mw: f64,
        qd_mvar: f64,
        in_service: bool,
        conforming: bool,
    ) -> Self {
        Self {
            bus,
            id: id.to_string(),
            pd_mw,
            qd_mvar,
            in_service,
            conforming,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "<Load bus={} id='{}' {:.1}MW {:.1}MVAr{}>",
            self.bus,
            self.id,
            self.pd_mw,
            self.qd_mvar,
            if self.in_service {
                ""
            } else {
                " OUT-OF-SERVICE"
            }
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BusSolved — Bus + Power Flow Solution
// ─────────────────────────────────────────────────────────────────────────────

/// Bus with power flow solution results.
///
/// Obtain via `sol.find_bus_numbers(net)`, `sol.bus(net, n)`.
#[pyclass(name = "BusSolved", skip_from_py_object)]
#[derive(Clone)]
pub struct BusSolved {
    // All static bus fields
    #[pyo3(get)]
    pub number: u32,
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub type_str: String,
    #[pyo3(get)]
    pub pd_mw: f64,
    #[pyo3(get)]
    pub qd_mvar: f64,
    #[pyo3(get)]
    pub gs_mw: f64,
    #[pyo3(get)]
    pub bs_mvar: f64,
    #[pyo3(get)]
    pub area: u32,
    #[pyo3(get)]
    pub zone: u32,
    #[pyo3(get)]
    pub base_kv: f64,
    #[pyo3(get)]
    pub vmin_pu: f64,
    #[pyo3(get)]
    pub vmax_pu: f64,
    #[pyo3(get)]
    pub latitude: Option<f64>,
    #[pyo3(get)]
    pub longitude: Option<f64>,
    // Solved power flow results (override initial values)
    /// Solved voltage magnitude (pu).
    #[pyo3(get)]
    pub vm_pu: f64,
    /// Solved voltage angle (degrees).
    #[pyo3(get)]
    pub va_deg: f64,
    /// Net active power injection (Pg - Pd) in MW.
    #[pyo3(get)]
    pub active_power_injection_pu_mw: f64,
    /// Net reactive power injection (Qg - Qd) in MVAr.
    #[pyo3(get)]
    pub reactive_power_injection_pu_mvar: f64,
    /// Real power load at this bus (MW). Sum of Load objects at this bus.
    #[pyo3(get)]
    pub p_load_mw: f64,
    /// Reactive power load at this bus (MVAr). Sum of Load objects at this bus.
    #[pyo3(get)]
    pub q_load_mvar: f64,
    /// Island assignment (0-indexed). 0 means island detection was not performed.
    #[pyo3(get)]
    pub island_id: usize,
    /// True if this bus hit a reactive power limit during solve (PV→PQ switched).
    #[pyo3(get)]
    pub q_limited: bool,
}

#[pymethods]
impl BusSolved {
    #[getter]
    fn is_slack(&self) -> bool {
        self.type_str == "Slack"
    }
    #[getter]
    fn is_pv(&self) -> bool {
        self.type_str == "PV"
    }
    #[getter]
    fn is_pq(&self) -> bool {
        self.type_str == "PQ"
    }
    #[getter]
    fn is_isolated(&self) -> bool {
        self.type_str == "Isolated"
    }
    #[getter]
    fn has_coordinates(&self) -> bool {
        self.latitude.is_some() && self.longitude.is_some()
    }
    /// Voltage in kV.
    #[getter]
    fn vm_kv(&self) -> f64 {
        self.vm_pu * self.base_kv
    }
    /// Voltage in rectangular form (Vr, Vi) in per-unit.
    #[getter]
    fn v_rect(&self) -> (f64, f64) {
        let va_rad = self.va_deg.to_radians();
        (self.vm_pu * va_rad.cos(), self.vm_pu * va_rad.sin())
    }
    /// Apparent net injection |S_inject| in MVA.
    #[getter]
    fn s_inject_mva(&self) -> f64 {
        (self.active_power_injection_pu_mw * self.active_power_injection_pu_mw
            + self.reactive_power_injection_pu_mvar * self.reactive_power_injection_pu_mvar)
            .sqrt()
    }
    /// Apparent load in MVA.
    #[getter]
    fn s_load_mva(&self) -> f64 {
        (self.pd_mw * self.pd_mw + self.qd_mvar * self.qd_mvar).sqrt()
    }
    /// True if voltage outside limits.
    #[getter]
    fn is_voltage_violated(&self) -> bool {
        self.vm_pu < self.vmin_pu || self.vm_pu > self.vmax_pu
    }
    /// Voltage deviation from nominal.
    #[getter]
    fn voltage_deviation_pu(&self) -> f64 {
        self.vm_pu - 1.0
    }

    fn __repr__(&self) -> String {
        format!(
            "<BusSolved {} '{}' V={:.4}∠{:.2}° P={:.1}MW Q={:.1}MVAr{}>",
            self.number,
            self.name,
            self.vm_pu,
            self.va_deg,
            self.active_power_injection_pu_mw,
            self.reactive_power_injection_pu_mvar,
            if self.q_limited { " [Q-LIM]" } else { "" }
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BranchSolved — Branch + Power Flow Flows
// ─────────────────────────────────────────────────────────────────────────────

/// Branch with power flow result flows.
///
/// Obtain via `sol.branches(net)`.
#[pyclass(name = "BranchSolved", skip_from_py_object)]
#[derive(Clone)]
pub struct BranchSolved {
    // All static branch fields
    #[pyo3(get)]
    pub from_bus: u32,
    #[pyo3(get)]
    pub to_bus: u32,
    #[pyo3(get)]
    pub circuit: String,
    #[pyo3(get)]
    pub r_pu: f64,
    #[pyo3(get)]
    pub x_pu: f64,
    #[pyo3(get)]
    pub b_pu: f64,
    #[pyo3(get)]
    pub rate_a_mva: f64,
    #[pyo3(get)]
    pub rate_b_mva: f64,
    #[pyo3(get)]
    pub rate_c_mva: f64,
    #[pyo3(get)]
    pub tap: f64,
    #[pyo3(get)]
    pub shift_deg: f64,
    #[pyo3(get)]
    pub in_service: bool,
    #[pyo3(get)]
    pub g_mag: f64,
    #[pyo3(get)]
    pub b_mag: f64,
    #[pyo3(get)]
    pub transformer_connection: String,
    /// Minimum angle difference (from-to) in degrees. None = unconstrained.
    #[pyo3(get)]
    pub angmin_deg: Option<f64>,
    /// Maximum angle difference (from-to) in degrees. None = unconstrained.
    #[pyo3(get)]
    pub angmax_deg: Option<f64>,
    // Solved branch flows
    /// From-end active power flow (MW).
    #[pyo3(get)]
    pub pf_mw: f64,
    /// From-end reactive power flow (MVAr).
    #[pyo3(get)]
    pub qf_mvar: f64,
    /// To-end active power flow (MW).
    #[pyo3(get)]
    pub pt_mw: f64,
    /// To-end reactive power flow (MVAr).
    #[pyo3(get)]
    pub qt_mvar: f64,
    /// Branch loading as % of Rate A (max-end |S| / rate_a * 100). 0 if rate_a = 0.
    #[pyo3(get)]
    pub loading_pct: f64,
    /// Active power losses on this branch (MW) = pf_mw + pt_mw.
    #[pyo3(get)]
    pub losses_mw: f64,
}

impl BranchSolved {
    /// Effective tap ratio, normalizing MATPOWER's tap=0 convention to 1.0.
    #[inline]
    fn effective_tap(&self) -> f64 {
        if self.tap.abs() < 1e-10 {
            1.0
        } else {
            self.tap
        }
    }
}

#[pymethods]
impl BranchSolved {
    #[getter]
    fn is_transformer(&self) -> bool {
        let tap = self.effective_tap();
        (tap - 1.0).abs() > 1e-6
            || self.shift_deg.abs() > 1e-6
            || self.g_mag.abs() > 1e-12
            || self.b_mag.abs() > 1e-12
            || self.transformer_connection != "WyeG-WyeG"
    }
    #[getter]
    fn b_dc_pu(&self) -> f64 {
        let tap = self.effective_tap();
        let d = self.x_pu * tap;
        if d.abs() < 1e-20 { 0.0 } else { 1.0 / d }
    }
    /// From-end apparent power (MVA).
    #[getter]
    fn sf_mva(&self) -> f64 {
        (self.pf_mw * self.pf_mw + self.qf_mvar * self.qf_mvar).sqrt()
    }
    /// To-end apparent power (MVA).
    #[getter]
    fn st_mva(&self) -> f64 {
        (self.pt_mw * self.pt_mw + self.qt_mvar * self.qt_mvar).sqrt()
    }
    /// Reactive power losses (MVAr) = qf + qt.
    #[getter]
    fn losses_mvar(&self) -> f64 {
        self.qf_mvar + self.qt_mvar
    }
    /// Headroom to rate A (MVA). NaN if rate_a == 0.
    #[getter]
    fn headroom_mva(&self) -> f64 {
        if self.rate_a_mva <= 0.0 {
            f64::NAN
        } else {
            self.rate_a_mva - self.sf_mva().max(self.st_mva())
        }
    }
    /// Headroom as percentage = 100 - loading_pct.
    #[getter]
    fn headroom_pct(&self) -> f64 {
        100.0 - self.loading_pct
    }
    /// True if loading > 100% (and rate_a > 0).
    #[getter]
    fn is_overloaded(&self) -> bool {
        self.rate_a_mva > 0.0 && self.loading_pct > 100.0
    }
    /// X/R ratio (f64::INFINITY if r_pu == 0).
    #[getter]
    fn x_r_ratio(&self) -> f64 {
        if self.r_pu.abs() < 1e-20 {
            f64::INFINITY
        } else {
            self.x_pu / self.r_pu
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "<BranchSolved {}→{} ckt={} Pf={:.1}MW loading={:.1}% loss={:.2}MW{}>",
            self.from_bus,
            self.to_bus,
            self.circuit,
            self.pf_mw,
            self.loading_pct,
            self.losses_mw,
            if self.in_service { "" } else { " OUT" }
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GenSolved — Generator + Power Flow Q result
// ─────────────────────────────────────────────────────────────────────────────

/// Generator with power flow solved reactive power output.
///
/// Obtain via `sol.generators(net)`.
#[pyclass(name = "GenSolved", skip_from_py_object)]
#[derive(Clone)]
pub struct GenSolved {
    // All static generator fields (same as Generator)
    #[pyo3(get)]
    pub bus: u32,
    #[pyo3(get)]
    pub machine_id: String,
    #[pyo3(get)]
    pub p_mw: f64,
    #[pyo3(get)]
    pub q_mvar: f64,
    #[pyo3(get)]
    pub pmax_mw: f64,
    #[pyo3(get)]
    pub pmin_mw: f64,
    #[pyo3(get)]
    pub qmax_mvar: f64,
    #[pyo3(get)]
    pub qmin_mvar: f64,
    #[pyo3(get)]
    pub vs_pu: f64,
    #[pyo3(get)]
    pub mbase_mva: f64,
    #[pyo3(get)]
    pub in_service: bool,
    #[pyo3(get)]
    pub fuel_type: Option<String>,
    #[pyo3(get)]
    pub heat_rate_btu_mwh: Option<f64>,
    #[pyo3(get)]
    pub co2_rate_t_per_mwh: f64,
    #[pyo3(get)]
    pub nox_rate_t_per_mwh: f64,
    #[pyo3(get)]
    pub so2_rate_t_per_mwh: f64,
    #[pyo3(get)]
    pub pm25_rate_t_per_mwh: f64,
    #[pyo3(get)]
    pub forced_outage_rate: Option<f64>,
    #[pyo3(get)]
    pub ramp_up_curve: Vec<(f64, f64)>,
    #[pyo3(get)]
    pub ramp_down_curve: Vec<(f64, f64)>,
    #[pyo3(get)]
    pub reg_ramp_up_curve: Vec<(f64, f64)>,
    #[pyo3(get)]
    pub ramp_up_mw_per_min: Option<f64>,
    #[pyo3(get)]
    pub ramp_dn_mw_per_min: Option<f64>,
    #[pyo3(get)]
    pub ramp_agc_mw_per_min: Option<f64>,
    #[pyo3(get)]
    pub commitment_status: String,
    #[pyo3(get)]
    pub min_up_time_hr: Option<f64>,
    #[pyo3(get)]
    pub min_down_time_hr: Option<f64>,
    #[pyo3(get)]
    pub startup_cost_tiers: Vec<(f64, f64, f64)>,
    #[pyo3(get)]
    pub quick_start: bool,
    #[pyo3(get)]
    pub reserve_offers: Vec<(String, f64, f64)>,
    #[pyo3(get)]
    pub qualifications: std::collections::HashMap<String, bool>,
    #[pyo3(get)]
    pub pc1_mw: Option<f64>,
    #[pyo3(get)]
    pub pc2_mw: Option<f64>,
    #[pyo3(get)]
    pub qc1min_mvar: Option<f64>,
    #[pyo3(get)]
    pub qc1max_mvar: Option<f64>,
    #[pyo3(get)]
    pub qc2min_mvar: Option<f64>,
    #[pyo3(get)]
    pub qc2max_mvar: Option<f64>,
    #[pyo3(get)]
    pub pq_curve: Vec<(f64, f64, f64)>,
    #[pyo3(get)]
    pub h_inertia_s: Option<f64>,
    #[pyo3(get)]
    pub xs_pu: Option<f64>,
    #[pyo3(get)]
    pub apf: Option<f64>,
    #[pyo3(get)]
    pub x2_pu: Option<f64>,
    #[pyo3(get)]
    pub zn_pu: Option<(f64, f64)>,
    #[pyo3(get)]
    pub cost_model: Option<String>,
    #[pyo3(get)]
    pub cost_startup: f64,
    #[pyo3(get)]
    pub cost_shutdown: f64,
    #[pyo3(get)]
    pub cost_coefficients: Vec<f64>,
    #[pyo3(get)]
    pub cost_breakpoints_mw: Vec<f64>,
    #[pyo3(get)]
    pub cost_breakpoints_usd: Vec<f64>,
    #[pyo3(get)]
    pub mu_pmin: Option<f64>,
    #[pyo3(get)]
    pub mu_pmax: Option<f64>,
    #[pyo3(get)]
    pub mu_qmin: Option<f64>,
    #[pyo3(get)]
    pub mu_qmax: Option<f64>,
    // Solved result
    /// Solved reactive power output (MVAr). Computed from PF reactive_power_injection_pu balance.
    #[pyo3(get)]
    pub q_mvar_solved: f64,
}

#[pymethods]
impl GenSolved {
    #[getter]
    fn has_cost(&self) -> bool {
        self.cost_model.is_some()
    }
    #[getter]
    fn cost_c0(&self) -> Option<f64> {
        if self.cost_model.as_deref() != Some("polynomial") {
            return None;
        }
        self.cost_coefficients.last().copied()
    }
    #[getter]
    fn cost_c1(&self) -> Option<f64> {
        if self.cost_model.as_deref() != Some("polynomial") {
            return None;
        }
        let n = self.cost_coefficients.len();
        if n >= 2 {
            Some(self.cost_coefficients[n - 2])
        } else {
            None
        }
    }
    #[getter]
    fn cost_c2(&self) -> Option<f64> {
        if self.cost_model.as_deref() != Some("polynomial") {
            return None;
        }
        let n = self.cost_coefficients.len();
        if n >= 3 {
            Some(self.cost_coefficients[n - 3])
        } else {
            None
        }
    }
    #[getter]
    fn has_reactive_capability_curve(&self) -> bool {
        !self.pq_curve.is_empty()
    }
    #[getter]
    fn must_run(&self) -> bool {
        self.commitment_status == "MustRun"
    }
    #[getter]
    fn has_ancillary_services(&self) -> bool {
        !self.reserve_offers.is_empty()
    }
    fn cost_at(&self, p_mw: f64) -> f64 {
        match self.cost_model.as_deref() {
            Some("polynomial") => eval_polynomial(&self.cost_coefficients, p_mw),
            Some("piecewise_linear") => {
                let pts: Vec<(f64, f64)> = self
                    .cost_breakpoints_mw
                    .iter()
                    .zip(self.cost_breakpoints_usd.iter())
                    .map(|(&m, &u)| (m, u))
                    .collect();
                eval_piecewise(&pts, p_mw)
            }
            _ => 0.0,
        }
    }
    fn marginal_cost_at(&self, p_mw: f64) -> f64 {
        match self.cost_model.as_deref() {
            Some("polynomial") => marginal_polynomial(&self.cost_coefficients, p_mw),
            Some("piecewise_linear") => {
                let pts: Vec<(f64, f64)> = self
                    .cost_breakpoints_mw
                    .iter()
                    .zip(self.cost_breakpoints_usd.iter())
                    .map(|(&m, &u)| (m, u))
                    .collect();
                marginal_piecewise(&pts, p_mw)
            }
            _ => 0.0,
        }
    }

    fn __repr__(&self) -> String {
        let fuel = self.fuel_type.as_deref().unwrap_or("?");
        format!(
            "<GenSolved bus={} id='{}' {} Pg={:.1}MW Qg={:.1}MVAr solved>",
            self.bus, self.machine_id, fuel, self.p_mw, self.q_mvar_solved
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BusOpf — Bus + OPF LMPs
// ─────────────────────────────────────────────────────────────────────────────

/// Bus with OPF locational marginal prices and voltage solution.
///
/// Obtain via `opf_result.find_bus_numbers(net)`.
#[pyclass(name = "BusOpf", skip_from_py_object)]
#[derive(Clone)]
pub struct BusOpf {
    // Static fields
    #[pyo3(get)]
    pub number: u32,
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub type_str: String,
    #[pyo3(get)]
    pub pd_mw: f64,
    #[pyo3(get)]
    pub qd_mvar: f64,
    #[pyo3(get)]
    pub area: u32,
    #[pyo3(get)]
    pub zone: u32,
    #[pyo3(get)]
    pub base_kv: f64,
    #[pyo3(get)]
    pub vmin_pu: f64,
    #[pyo3(get)]
    pub vmax_pu: f64,
    // OPF voltage solution
    #[pyo3(get)]
    pub vm_pu: f64,
    #[pyo3(get)]
    pub va_deg: f64,
    // LMP decomposition
    /// Locational marginal price ($/MWh).
    #[pyo3(get)]
    pub lmp: f64,
    /// Energy component of LMP ($/MWh).
    #[pyo3(get)]
    pub lmp_energy: f64,
    /// Congestion component of LMP ($/MWh).
    #[pyo3(get)]
    pub lmp_congestion: f64,
    /// Loss component of LMP ($/MWh). Zero for DC-OPF.
    #[pyo3(get)]
    pub lmp_loss: f64,
    /// Reactive LMP ($/MVAr-h). Zero for DC-OPF.
    #[pyo3(get)]
    pub lmp_reactive: f64,
    /// Shadow price on Vmin constraint ($/MWh per pu). Zero if not binding or DC-OPF.
    #[pyo3(get)]
    pub mu_vmin: f64,
    /// Shadow price on Vmax constraint ($/MWh per pu). Zero if not binding or DC-OPF.
    #[pyo3(get)]
    pub mu_vmax: f64,
}

#[pymethods]
impl BusOpf {
    #[getter]
    fn is_slack(&self) -> bool {
        self.type_str == "Slack"
    }
    #[getter]
    fn is_pv(&self) -> bool {
        self.type_str == "PV"
    }
    #[getter]
    fn is_pq(&self) -> bool {
        self.type_str == "PQ"
    }
    /// Load payment at LMP ($/hr) = lmp * pd_mw.
    #[getter]
    fn load_payment_per_hr(&self) -> f64 {
        self.lmp * self.pd_mw
    }
    /// True if LMP congestion component is significant (> 1 $/MWh).
    #[getter]
    fn is_congested(&self) -> bool {
        self.lmp_congestion.abs() > 1.0
    }
    /// True if voltage limit constraint is binding (shadow price > 1e-6).
    #[getter]
    fn is_voltage_constrained(&self) -> bool {
        self.mu_vmin.abs() > 1e-6 || self.mu_vmax.abs() > 1e-6
    }

    fn __repr__(&self) -> String {
        format!(
            "<BusOpf {} '{}' V={:.4}pu LMP={:.2}$/MWh (E={:.2} C={:.2} L={:.2})>",
            self.number,
            self.name,
            self.vm_pu,
            self.lmp,
            self.lmp_energy,
            self.lmp_congestion,
            self.lmp_loss
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BranchOpf — Branch + OPF flows and shadow prices
// ─────────────────────────────────────────────────────────────────────────────

/// Branch with OPF power flows and shadow prices.
///
/// Obtain via `opf_result.branches(net)`.
#[pyclass(name = "BranchOpf", skip_from_py_object)]
#[derive(Clone)]
pub struct BranchOpf {
    // Static fields
    #[pyo3(get)]
    pub from_bus: u32,
    #[pyo3(get)]
    pub to_bus: u32,
    #[pyo3(get)]
    pub circuit: String,
    #[pyo3(get)]
    pub r_pu: f64,
    #[pyo3(get)]
    pub x_pu: f64,
    #[pyo3(get)]
    pub b_pu: f64,
    #[pyo3(get)]
    pub rate_a_mva: f64,
    /// Short-term MVA rating.
    #[pyo3(get)]
    pub rate_b_mva: f64,
    /// Emergency MVA rating.
    #[pyo3(get)]
    pub rate_c_mva: f64,
    #[pyo3(get)]
    pub tap: f64,
    #[pyo3(get)]
    pub shift_deg: f64,
    #[pyo3(get)]
    pub in_service: bool,
    /// Transformer magnetizing conductance (pu).
    #[pyo3(get)]
    pub g_mag: f64,
    /// Transformer magnetizing susceptance (pu).
    #[pyo3(get)]
    pub b_mag: f64,
    /// Zero-sequence winding connection type.
    #[pyo3(get)]
    pub transformer_connection: String,
    /// Minimum angle difference in degrees. None = unconstrained.
    #[pyo3(get)]
    pub angmin_deg: Option<f64>,
    /// Maximum angle difference in degrees. None = unconstrained.
    #[pyo3(get)]
    pub angmax_deg: Option<f64>,
    // OPF flows
    #[pyo3(get)]
    pub pf_mw: f64,
    #[pyo3(get)]
    pub qf_mvar: f64, // 0 for DC-OPF
    #[pyo3(get)]
    pub pt_mw: f64,
    #[pyo3(get)]
    pub qt_mvar: f64, // 0 for DC-OPF
    #[pyo3(get)]
    pub loading_pct: f64,
    /// Active power losses (MW) = pf_mw + pt_mw.
    #[pyo3(get)]
    pub losses_mw: f64,
    // Shadow prices
    /// Shadow price on thermal flow limit ($/MWh per MW). Positive = forward binding, negative = reverse.
    #[pyo3(get)]
    pub shadow_price: f64,
    /// Shadow price on angmin constraint ($/MWh per rad). Zero if unconstrained.
    #[pyo3(get)]
    pub mu_angmin: f64,
    /// Shadow price on angmax constraint ($/MWh per rad). Zero if unconstrained.
    #[pyo3(get)]
    pub mu_angmax: f64,
}

impl BranchOpf {
    /// Effective tap ratio, normalizing MATPOWER's tap=0 convention to 1.0.
    #[inline]
    fn effective_tap(&self) -> f64 {
        if self.tap.abs() < 1e-10 {
            1.0
        } else {
            self.tap
        }
    }
}

#[pymethods]
impl BranchOpf {
    /// True if the thermal constraint is binding (|shadow_price| > 1e-6).
    #[getter]
    fn is_binding(&self) -> bool {
        self.shadow_price.abs() > 1e-6
    }
    #[getter]
    fn is_transformer(&self) -> bool {
        let tap = self.effective_tap();
        (tap - 1.0).abs() > 1e-6
            || self.shift_deg.abs() > 1e-6
            || self.g_mag.abs() > 1e-12
            || self.b_mag.abs() > 1e-12
            || self.transformer_connection != "WyeG-WyeG"
    }
    /// From-end apparent power (MVA).
    #[getter]
    fn sf_mva(&self) -> f64 {
        (self.pf_mw * self.pf_mw + self.qf_mvar * self.qf_mvar).sqrt()
    }
    /// To-end apparent power (MVA).
    #[getter]
    fn st_mva(&self) -> f64 {
        (self.pt_mw * self.pt_mw + self.qt_mvar * self.qt_mvar).sqrt()
    }
    /// Headroom to rate A (MVA). NaN if rate_a == 0.
    #[getter]
    fn headroom_mva(&self) -> f64 {
        if self.rate_a_mva <= 0.0 {
            f64::NAN
        } else {
            self.rate_a_mva - self.sf_mva().max(self.st_mva())
        }
    }

    fn __repr__(&self) -> String {
        let bind = if self.is_binding() {
            format!(" BINDING μ={:.2}", self.shadow_price)
        } else {
            String::new()
        };
        format!(
            "<BranchOpf {}→{} Pf={:.1}MW loading={:.1}%{}>",
            self.from_bus, self.to_bus, self.pf_mw, self.loading_pct, bind
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GenOpf — Generator + OPF dispatch and duals
// ─────────────────────────────────────────────────────────────────────────────

/// Generator with OPF dispatch and KKT multipliers.
///
/// Obtain via `opf_result.generators(net)`.
#[pyclass(name = "GenOpf", skip_from_py_object)]
#[derive(Clone)]
pub struct GenOpf {
    // Static fields
    #[pyo3(get)]
    pub bus: u32,
    #[pyo3(get)]
    pub machine_id: String,
    #[pyo3(get)]
    pub pmax_mw: f64,
    #[pyo3(get)]
    pub pmin_mw: f64,
    #[pyo3(get)]
    pub qmax_mvar: f64,
    #[pyo3(get)]
    pub qmin_mvar: f64,
    #[pyo3(get)]
    pub in_service: bool,
    #[pyo3(get)]
    pub fuel_type: Option<String>,
    // OPF dispatch (replaces model pg/qg)
    /// Optimal active power dispatch (MW).
    #[pyo3(get)]
    pub p_mw: f64,
    /// Optimal reactive power dispatch (MVAr). 0 for DC-OPF.
    #[pyo3(get)]
    pub q_mvar: f64,
    // KKT multipliers
    /// Shadow price on Pmin constraint ($/MWh). Positive = generator at lower limit.
    #[pyo3(get)]
    pub mu_pmin: f64,
    /// Shadow price on Pmax constraint ($/MWh). Positive = generator at upper limit.
    #[pyo3(get)]
    pub mu_pmax: f64,
    /// Shadow price on Qmin constraint ($/MWh). 0 for DC-OPF.
    #[pyo3(get)]
    pub mu_qmin: f64,
    /// Shadow price on Qmax constraint ($/MWh). 0 for DC-OPF.
    #[pyo3(get)]
    pub mu_qmax: f64,
    /// Voltage setpoint (pu).
    #[pyo3(get)]
    pub vs_pu: f64,
    /// Machine base MVA.
    #[pyo3(get)]
    pub mbase_mva: f64,
    /// CO2 emission rate (tonnes/MWh).
    #[pyo3(get)]
    pub co2_rate_t_per_mwh: f64,
    // Cost fields (for computing actual dispatch cost and exposing to Python)
    #[pyo3(get)]
    pub cost_model: Option<String>,
    #[pyo3(get)]
    pub cost_coefficients: Vec<f64>,
    /// Piecewise breakpoints MW values.
    #[pyo3(get)]
    pub cost_breakpoints_mw: Vec<f64>,
    /// Piecewise breakpoints $/hr values.
    #[pyo3(get)]
    pub cost_breakpoints_usd: Vec<f64>,
    /// Startup cost ($/event). 0 if no cost data.
    #[pyo3(get)]
    pub cost_startup: f64,
    /// Shutdown cost ($). 0 if no cost data.
    #[pyo3(get)]
    pub cost_shutdown: f64,
}

#[pymethods]
impl GenOpf {
    /// True if this generator has a cost curve.
    #[getter]
    fn has_cost(&self) -> bool {
        self.cost_model.is_some()
    }

    /// Constant term c₀ of a polynomial cost curve ($/hr at P=0).
    ///
    /// Returns `None` if the cost model is not polynomial.
    #[getter]
    fn cost_c0(&self) -> Option<f64> {
        if self.cost_model.as_deref() != Some("polynomial") {
            return None;
        }
        self.cost_coefficients.last().copied()
    }
    /// Linear coefficient c₁ of a polynomial cost curve ($/MWh at P=0).
    ///
    /// Returns `None` if the cost model is not polynomial or has fewer than 2 coefficients.
    #[getter]
    fn cost_c1(&self) -> Option<f64> {
        if self.cost_model.as_deref() != Some("polynomial") {
            return None;
        }
        let n = self.cost_coefficients.len();
        if n >= 2 {
            Some(self.cost_coefficients[n - 2])
        } else {
            None
        }
    }
    /// Quadratic coefficient c₂ of a polynomial cost curve ($/MW²·hr).
    ///
    /// Returns `None` if the cost model is not polynomial or has fewer than 3 coefficients.
    #[getter]
    fn cost_c2(&self) -> Option<f64> {
        if self.cost_model.as_deref() != Some("polynomial") {
            return None;
        }
        let n = self.cost_coefficients.len();
        if n >= 3 {
            Some(self.cost_coefficients[n - 3])
        } else {
            None
        }
    }

    /// Actual dispatch cost at optimal p_mw ($/hr).
    #[getter]
    fn cost_actual(&self) -> f64 {
        match self.cost_model.as_deref() {
            Some("polynomial") => eval_polynomial(&self.cost_coefficients, self.p_mw),
            Some("piecewise_linear") => {
                let pts: Vec<(f64, f64)> = self
                    .cost_breakpoints_mw
                    .iter()
                    .zip(self.cost_breakpoints_usd.iter())
                    .map(|(&m, &u)| (m, u))
                    .collect();
                eval_piecewise(&pts, self.p_mw)
            }
            _ => 0.0,
        }
    }

    /// Total cost ($/hr) at a given dispatch level.
    fn cost_at(&self, p_mw: f64) -> f64 {
        match self.cost_model.as_deref() {
            Some("polynomial") => eval_polynomial(&self.cost_coefficients, p_mw),
            Some("piecewise_linear") => {
                let pts: Vec<(f64, f64)> = self
                    .cost_breakpoints_mw
                    .iter()
                    .zip(self.cost_breakpoints_usd.iter())
                    .map(|(&m, &u)| (m, u))
                    .collect();
                eval_piecewise(&pts, p_mw)
            }
            _ => 0.0,
        }
    }

    /// Marginal cost ($/MWh) at a given dispatch level.
    fn marginal_cost_at(&self, p_mw: f64) -> f64 {
        match self.cost_model.as_deref() {
            Some("polynomial") => marginal_polynomial(&self.cost_coefficients, p_mw),
            Some("piecewise_linear") => {
                let pts: Vec<(f64, f64)> = self
                    .cost_breakpoints_mw
                    .iter()
                    .zip(self.cost_breakpoints_usd.iter())
                    .map(|(&m, &u)| (m, u))
                    .collect();
                marginal_piecewise(&pts, p_mw)
            }
            _ => 0.0,
        }
    }

    /// Dispatch as percentage of pmax (NaN if pmax == 0).
    #[getter]
    fn dispatch_pct(&self) -> f64 {
        if self.pmax_mw.abs() < 1e-10 {
            f64::NAN
        } else {
            self.p_mw / self.pmax_mw * 100.0
        }
    }
    /// Active power headroom (pmax - p_mw).
    #[getter]
    fn headroom_mw(&self) -> f64 {
        self.pmax_mw - self.p_mw
    }
    /// True if generator is at its upper active power limit.
    #[getter]
    fn is_at_pmax(&self) -> bool {
        self.mu_pmax > 1e-4 || (self.pmax_mw - self.p_mw) < 0.01
    }
    /// True if generator is at its lower active power limit.
    #[getter]
    fn is_at_pmin(&self) -> bool {
        self.mu_pmin > 1e-4 || (self.p_mw - self.pmin_mw) < 0.01
    }

    fn __repr__(&self) -> String {
        let fuel = self.fuel_type.as_deref().unwrap_or("?");
        let limit = if self.is_at_pmax() {
            " @Pmax"
        } else if self.is_at_pmin() {
            " @Pmin"
        } else {
            ""
        };
        format!(
            "<GenOpf bus={} id='{}' {} Pg={:.1}MW ({:.0}%) cost={:.1}$/hr{}>",
            self.bus,
            self.machine_id,
            fuel,
            self.p_mw,
            self.dispatch_pct(),
            self.cost_actual(),
            limit
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Builder functions: Network → Vec<PyXxx>
// ─────────────────────────────────────────────────────────────────────────────

/// Check if a Branch is a transformer (callable from Rust, since pymethods are private).
pub fn branch_is_transformer(br: &Branch) -> bool {
    let tap = br.effective_tap();
    (tap - 1.0).abs() > 1e-6
        || br.shift_deg.abs() > 1e-6
        || br.g_mag.abs() > 1e-12
        || br.b_mag.abs() > 1e-12
        || br.transformer_connection != "WyeG-WyeG"
}

/// Check if a core branch is a transformer (predicate on `CoreBranch`).
pub fn core_branch_is_transformer(br: &CoreBranch) -> bool {
    use surge_network::network::TransformerConnection;
    let tap = if br.tap.abs() < 1e-10 { 1.0 } else { br.tap };
    (tap - 1.0).abs() > 1e-6
        || br.phase_shift_rad.abs() > 1e-6
        || br.g_mag.abs() > 1e-12
        || br.b_mag.abs() > 1e-12
        || br
            .transformer_data
            .as_ref()
            .map(|t| t.transformer_connection)
            .unwrap_or_default()
            != TransformerConnection::WyeGWyeG
}

/// Build all Bus objects from a network.
pub fn buses_from_network(net: &CoreNetwork) -> Vec<Bus> {
    let load_p = net.bus_load_p_mw();
    let load_q = net.bus_load_q_mvar();
    net.buses
        .iter()
        .enumerate()
        .map(|(i, b)| {
            Bus::from_core_with_load(
                b,
                load_p.get(i).copied().unwrap_or(0.0),
                load_q.get(i).copied().unwrap_or(0.0),
            )
        })
        .collect()
}

/// Build all Branch objects from a network.
pub fn branches_from_network(net: &CoreNetwork) -> Vec<Branch> {
    net.branches.iter().map(Branch::from_core).collect()
}

/// Build Branch objects from a network, only for core branches matching a predicate.
pub fn branches_filtered(net: &CoreNetwork, pred: impl Fn(&CoreBranch) -> bool) -> Vec<Branch> {
    net.branches
        .iter()
        .filter(|b| pred(b))
        .map(Branch::from_core)
        .collect()
}

/// Build all Generator objects from a network.
pub fn generators_from_network(net: &CoreNetwork) -> Vec<Generator> {
    net.generators.iter().map(Generator::from_core).collect()
}

/// Build Generator objects from a network, only for core generators matching a predicate.
pub fn generators_filtered(
    net: &CoreNetwork,
    pred: impl Fn(&CoreGenerator) -> bool,
) -> Vec<Generator> {
    net.generators
        .iter()
        .filter(|g| pred(g))
        .map(Generator::from_core)
        .collect()
}

/// Build all Load objects from a network.
pub fn loads_from_network(net: &CoreNetwork) -> Vec<Load> {
    net.loads.iter().map(Load::from_core).collect()
}

/// Build Load objects from a network, only for core loads matching a predicate.
pub fn loads_filtered(net: &CoreNetwork, pred: impl Fn(&CoreLoad) -> bool) -> Vec<Load> {
    net.loads
        .iter()
        .filter(|l| pred(l))
        .map(Load::from_core)
        .collect()
}

/// Find a bus by external bus number.
pub fn find_bus(net: &CoreNetwork, number: u32) -> PyResult<Bus> {
    let load_p = net.bus_load_p_mw();
    let load_q = net.bus_load_q_mvar();
    net.buses
        .iter()
        .enumerate()
        .find(|(_, b)| b.number == number)
        .map(|(i, b)| {
            Bus::from_core_with_load(
                b,
                load_p.get(i).copied().unwrap_or(0.0),
                load_q.get(i).copied().unwrap_or(0.0),
            )
        })
        .ok_or_else(|| NetworkError::new_err(format!("Bus {number} not found")))
}

/// Find a branch by (from_bus, to_bus, circuit).
pub fn find_branch(
    net: &CoreNetwork,
    from_bus: u32,
    to_bus: u32,
    circuit: &str,
) -> PyResult<Branch> {
    net.branches
        .iter()
        .find(|br| br.from_bus == from_bus && br.to_bus == to_bus && br.circuit == circuit)
        .or_else(|| {
            // try reversed direction
            net.branches
                .iter()
                .find(|br| br.from_bus == to_bus && br.to_bus == from_bus && br.circuit == circuit)
        })
        .map(Branch::from_core)
        .ok_or_else(|| {
            NetworkError::new_err(format!(
                "Branch {from_bus}→{to_bus} ckt={circuit} not found"
            ))
        })
}

/// Find a generator by canonical generator ID.
pub fn find_generator_by_id(net: &CoreNetwork, id: &str) -> PyResult<Generator> {
    net.generators
        .iter()
        .find(|g| g.id == id)
        .map(Generator::from_core)
        .ok_or_else(|| NetworkError::new_err(format!("Generator id='{id}' not found")))
}

/// Get the slack bus.
pub fn find_slack_bus(net: &CoreNetwork) -> PyResult<Bus> {
    let load_p = net.bus_load_p_mw();
    let load_q = net.bus_load_q_mvar();
    net.buses
        .iter()
        .enumerate()
        .find(|(_, b)| b.bus_type == BusType::Slack)
        .map(|(i, b)| {
            Bus::from_core_with_load(
                b,
                load_p.get(i).copied().unwrap_or(0.0),
                load_q.get(i).copied().unwrap_or(0.0),
            )
        })
        .ok_or_else(|| NetworkError::new_err("No Slack bus found in network"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Builder functions: AcPfResult + Network → Vec<PyXxxSolved>
// ─────────────────────────────────────────────────────────────────────────────

/// Build BusSolved objects by merging network data with PF solution.
pub fn buses_solved(pf: &CorePfSolution, net: &CoreNetwork) -> Vec<BusSolved> {
    let base = net.base_mva;
    let load_p = net.bus_load_p_mw();
    let load_q = net.bus_load_q_mvar();
    // Build external → internal index map
    let bus_ext_to_idx: std::collections::HashMap<u32, usize> = pf
        .bus_numbers
        .iter()
        .enumerate()
        .map(|(i, &n)| (n, i))
        .collect();
    let q_limited_set: std::collections::HashSet<u32> =
        pf.q_limited_buses.iter().copied().collect();

    net.buses
        .iter()
        .enumerate()
        .map(|(bus_idx, b)| {
            let pd = load_p.get(bus_idx).copied().unwrap_or(0.0);
            let qd = load_q.get(bus_idx).copied().unwrap_or(0.0);
            let idx = bus_ext_to_idx.get(&b.number).copied();
            let (vm, va, p_inj, q_inj, island_id) = if let Some(i) = idx {
                let vm = *pf
                    .voltage_magnitude_pu
                    .get(i)
                    .unwrap_or(&b.voltage_magnitude_pu);
                let va = pf
                    .voltage_angle_rad
                    .get(i)
                    .copied()
                    .unwrap_or(b.voltage_angle_rad)
                    .to_degrees();
                let p = pf.active_power_injection_pu.get(i).copied().unwrap_or(0.0) * base;
                let q = pf
                    .reactive_power_injection_pu
                    .get(i)
                    .copied()
                    .unwrap_or(0.0)
                    * base;
                let isl = pf.island_ids.get(i).copied().unwrap_or(0);
                (vm, va, p, q, isl)
            } else {
                (
                    b.voltage_magnitude_pu,
                    b.voltage_angle_rad.to_degrees(),
                    0.0,
                    0.0,
                    0,
                )
            };
            BusSolved {
                number: b.number,
                name: b.name.clone(),
                type_str: bus_type_str(b.bus_type),
                pd_mw: pd,
                qd_mvar: qd,
                gs_mw: b.shunt_conductance_mw,
                bs_mvar: b.shunt_susceptance_mvar,
                area: b.area,
                zone: b.zone,
                base_kv: b.base_kv,
                vmin_pu: b.voltage_min_pu,
                vmax_pu: b.voltage_max_pu,
                latitude: b.latitude,
                longitude: b.longitude,
                vm_pu: vm,
                va_deg: va,
                active_power_injection_pu_mw: p_inj,
                reactive_power_injection_pu_mvar: q_inj,
                p_load_mw: pd,
                q_load_mvar: qd,
                island_id,
                q_limited: q_limited_set.contains(&b.number),
            }
        })
        .collect()
}

/// Build a single BusSolved by bus number.
pub fn bus_solved(pf: &CorePfSolution, net: &CoreNetwork, number: u32) -> PyResult<BusSolved> {
    buses_solved(pf, net)
        .into_iter()
        .find(|b| b.number == number)
        .ok_or_else(|| NetworkError::new_err(format!("Bus {number} not found")))
}

/// Build BranchSolved objects by merging network data with PF solution flows.
pub fn branches_solved(pf: &CorePfSolution, net: &CoreNetwork) -> Vec<BranchSolved> {
    let pq_flows = pf.branch_pq_flows();
    let all_flows = compute_to_end_flows(pf, net);

    net.branches
        .iter()
        .enumerate()
        .map(|(i, br)| {
            use surge_network::network::TransformerConnection;
            let (pf_mw, qf_mvar) = pq_flows.get(i).copied().unwrap_or((0.0, 0.0));
            let (pt_mw, qt_mvar) = all_flows.get(i).copied().unwrap_or((0.0, 0.0));
            let sf_mva = (pf_mw * pf_mw + qf_mvar * qf_mvar).sqrt();
            let st_mva = (pt_mw * pt_mw + qt_mvar * qt_mvar).sqrt();
            let s_max = sf_mva.max(st_mva);
            let loading_pct = if br.rating_a_mva > 0.0 {
                s_max / br.rating_a_mva * 100.0
            } else {
                0.0
            };
            let losses_mw = pf_mw + pt_mw;
            BranchSolved {
                from_bus: br.from_bus,
                to_bus: br.to_bus,
                circuit: br.circuit.clone(),
                r_pu: br.r,
                x_pu: br.x,
                b_pu: br.b,
                rate_a_mva: br.rating_a_mva,
                rate_b_mva: br.rating_b_mva,
                rate_c_mva: br.rating_c_mva,
                tap: br.tap,
                shift_deg: br.phase_shift_rad.to_degrees(),
                in_service: br.in_service,
                g_mag: br.g_mag,
                b_mag: br.b_mag,
                transformer_connection: match br
                    .transformer_data
                    .as_ref()
                    .map(|t| t.transformer_connection)
                    .unwrap_or_default()
                {
                    TransformerConnection::WyeGWyeG => "WyeG-WyeG".to_string(),
                    TransformerConnection::WyeGDelta => "WyeG-Delta".to_string(),
                    TransformerConnection::DeltaWyeG => "Delta-WyeG".to_string(),
                    TransformerConnection::DeltaDelta => "Delta-Delta".to_string(),
                    TransformerConnection::WyeGWye => "WyeG-Wye".to_string(),
                },
                angmin_deg: br.angle_diff_min_rad.map(|a| a.to_degrees()),
                angmax_deg: br.angle_diff_max_rad.map(|a| a.to_degrees()),
                pf_mw,
                qf_mvar,
                pt_mw,
                qt_mvar,
                loading_pct,
                losses_mw,
            }
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// BusDcSolved — Bus + DC Power Flow Angles
// ─────────────────────────────────────────────────────────────────────────────

/// Bus with DC power flow results.
///
/// Obtain via `dc_sol.buses`.
#[pyclass(name = "BusDcSolved", skip_from_py_object)]
#[derive(Clone)]
pub struct BusDcSolved {
    #[pyo3(get)]
    pub number: u32,
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub type_str: String,
    #[pyo3(get)]
    pub pd_mw: f64,
    #[pyo3(get)]
    pub qd_mvar: f64,
    #[pyo3(get)]
    pub area: u32,
    #[pyo3(get)]
    pub zone: u32,
    #[pyo3(get)]
    pub base_kv: f64,
    /// Solved voltage angle (radians).
    #[pyo3(get)]
    pub theta_rad: f64,
    /// Solved voltage angle (degrees).
    #[pyo3(get)]
    pub theta_deg: f64,
}

#[pymethods]
impl BusDcSolved {
    fn __repr__(&self) -> String {
        format!(
            "<BusDcSolved {} '{}' theta={:.4} rad ({:.2}\u{00b0})>",
            self.number, self.name, self.theta_rad, self.theta_deg,
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BranchDcSolved — Branch + DC Power Flow Flows
// ─────────────────────────────────────────────────────────────────────────────

/// Branch with DC power flow result flows.
///
/// Obtain via `dc_sol.branches`.
#[pyclass(name = "BranchDcSolved", skip_from_py_object)]
#[derive(Clone)]
pub struct BranchDcSolved {
    #[pyo3(get)]
    pub from_bus: u32,
    #[pyo3(get)]
    pub to_bus: u32,
    #[pyo3(get)]
    pub circuit: String,
    #[pyo3(get)]
    pub r_pu: f64,
    #[pyo3(get)]
    pub x_pu: f64,
    #[pyo3(get)]
    pub b_pu: f64,
    #[pyo3(get)]
    pub rate_a_mva: f64,
    #[pyo3(get)]
    pub tap: f64,
    #[pyo3(get)]
    pub shift_deg: f64,
    #[pyo3(get)]
    pub in_service: bool,
    /// Active power flow (MW).
    #[pyo3(get)]
    pub flow_mw: f64,
    /// Branch loading as % of Rate A (|flow_mw| / rate_a * 100). 0 if rate_a = 0.
    #[pyo3(get)]
    pub loading_pct: f64,
}

#[pymethods]
impl BranchDcSolved {
    fn __repr__(&self) -> String {
        format!(
            "<BranchDcSolved {}->{} ckt='{}' flow={:.1} MW loading={:.1}%>",
            self.from_bus, self.to_bus, self.circuit, self.flow_mw, self.loading_pct,
        )
    }
}

/// Build DC-solved bus objects from theta vector and network.
pub fn buses_dc_solved(theta: &[f64], bus_numbers: &[u32], net: &CoreNetwork) -> Vec<BusDcSolved> {
    let load_p = net.bus_load_p_mw();
    let load_q = net.bus_load_q_mvar();
    let bus_ext_to_idx: std::collections::HashMap<u32, usize> = bus_numbers
        .iter()
        .enumerate()
        .map(|(i, &n)| (n, i))
        .collect();

    net.buses
        .iter()
        .enumerate()
        .map(|(bus_idx, b)| {
            let theta_rad = bus_ext_to_idx
                .get(&b.number)
                .and_then(|&i| theta.get(i))
                .copied()
                .unwrap_or(0.0);
            BusDcSolved {
                number: b.number,
                name: b.name.clone(),
                type_str: bus_type_str(b.bus_type),
                pd_mw: load_p.get(bus_idx).copied().unwrap_or(0.0),
                qd_mvar: load_q.get(bus_idx).copied().unwrap_or(0.0),
                area: b.area,
                zone: b.zone,
                base_kv: b.base_kv,
                theta_rad,
                theta_deg: theta_rad.to_degrees(),
            }
        })
        .collect()
}

/// Build DC-solved branch objects from flow vector and network.
pub fn branches_dc_solved(flows_mw: &[f64], net: &CoreNetwork) -> Vec<BranchDcSolved> {
    net.branches
        .iter()
        .enumerate()
        .map(|(i, br)| {
            let flow_mw = flows_mw.get(i).copied().unwrap_or(0.0);
            let loading_pct = if br.rating_a_mva > 0.0 {
                flow_mw.abs() / br.rating_a_mva * 100.0
            } else {
                0.0
            };
            BranchDcSolved {
                from_bus: br.from_bus,
                to_bus: br.to_bus,
                circuit: br.circuit.clone(),
                r_pu: br.r,
                x_pu: br.x,
                b_pu: br.b,
                rate_a_mva: br.rating_a_mva,
                tap: br.tap,
                shift_deg: br.phase_shift_rad.to_degrees(),
                in_service: br.in_service,
                flow_mw,
                loading_pct,
            }
        })
        .collect()
}

/// Build GenSolved objects (generator + solved Qg).
pub fn generators_solved(pf: &CorePfSolution, net: &CoreNetwork) -> Vec<GenSolved> {
    // Compute per-generator solved Qg using the same logic as AcPfResult::gen_q_mvar.
    // reactive_power_injection_pu (pu) → Qg_bus (MVAr): Qg = reactive_power_injection_pu * base + Qd - Bs * Vm²
    // Multiple generators at the same bus are apportioned by their (Qmax - Qmin) range.
    let base = net.base_mva;
    let bus_map = net.bus_index_map();
    let load_q = net.bus_load_q_mvar();

    let mut bus_qg: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
    for (bus_idx, bus) in net.buses.iter().enumerate() {
        let idx = bus_map[&bus.number];
        if idx < pf.reactive_power_injection_pu.len() {
            let vm = pf.voltage_magnitude_pu[idx];
            let qd = load_q.get(bus_idx).copied().unwrap_or(0.0);
            let qg_bus = pf.reactive_power_injection_pu[idx] * base + qd
                - bus.shunt_susceptance_mvar * vm * vm;
            bus_qg.insert(bus.number, qg_bus);
        }
    }

    let mut bus_range: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
    for g in &net.generators {
        if g.in_service {
            *bus_range.entry(g.bus).or_insert(0.0) += (g.qmax - g.qmin).max(0.0);
        }
    }

    net.generators
        .iter()
        .map(|g| {
            let q_mvar_solved = if !g.in_service {
                0.0
            } else {
                let total_qg = bus_qg.get(&g.bus).copied().unwrap_or(0.0);
                let range = bus_range.get(&g.bus).copied().unwrap_or(0.0);
                let gen_range = (g.qmax - g.qmin).max(0.0);
                if range > 1e-6 {
                    total_qg * gen_range / range
                } else {
                    total_qg
                }
            };
            let pyg = Generator::from_core(g);
            GenSolved {
                bus: pyg.bus,
                machine_id: pyg.machine_id,
                p_mw: pyg.p_mw,
                q_mvar: pyg.q_mvar,
                pmax_mw: pyg.pmax_mw,
                pmin_mw: pyg.pmin_mw,
                qmax_mvar: pyg.qmax_mvar,
                qmin_mvar: pyg.qmin_mvar,
                vs_pu: pyg.vs_pu,
                mbase_mva: pyg.mbase_mva,
                in_service: pyg.in_service,
                fuel_type: pyg.fuel_type,
                heat_rate_btu_mwh: pyg.heat_rate_btu_mwh,
                co2_rate_t_per_mwh: pyg.co2_rate_t_per_mwh,
                nox_rate_t_per_mwh: pyg.nox_rate_t_per_mwh,
                so2_rate_t_per_mwh: pyg.so2_rate_t_per_mwh,
                pm25_rate_t_per_mwh: pyg.pm25_rate_t_per_mwh,
                forced_outage_rate: pyg.forced_outage_rate,
                ramp_up_curve: pyg.ramp_up_curve,
                ramp_down_curve: pyg.ramp_down_curve,
                reg_ramp_up_curve: pyg.reg_ramp_up_curve,
                ramp_up_mw_per_min: pyg.ramp_up_mw_per_min,
                ramp_dn_mw_per_min: pyg.ramp_dn_mw_per_min,
                ramp_agc_mw_per_min: pyg.ramp_agc_mw_per_min,
                commitment_status: pyg.commitment_status,
                min_up_time_hr: pyg.min_up_time_hr,
                min_down_time_hr: pyg.min_down_time_hr,
                startup_cost_tiers: pyg.startup_cost_tiers,
                quick_start: pyg.quick_start,
                reserve_offers: pyg.reserve_offers,
                qualifications: pyg.qualifications,
                pc1_mw: pyg.pc1_mw,
                pc2_mw: pyg.pc2_mw,
                qc1min_mvar: pyg.qc1min_mvar,
                qc1max_mvar: pyg.qc1max_mvar,
                qc2min_mvar: pyg.qc2min_mvar,
                qc2max_mvar: pyg.qc2max_mvar,
                pq_curve: pyg.pq_curve,
                h_inertia_s: pyg.h_inertia_s,
                xs_pu: pyg.xs_pu,
                apf: pyg.apf,
                x2_pu: pyg.x2_pu,
                zn_pu: pyg.zn_pu,
                cost_model: pyg.cost_model,
                cost_startup: pyg.cost_startup,
                cost_shutdown: pyg.cost_shutdown,
                cost_coefficients: pyg.cost_coefficients,
                cost_breakpoints_mw: pyg.cost_breakpoints_mw,
                cost_breakpoints_usd: pyg.cost_breakpoints_usd,
                mu_pmin: None,
                mu_pmax: None,
                mu_qmin: None,
                mu_qmax: None,
                q_mvar_solved,
            }
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Builder functions: OpfSolution + Network → Vec<PyXxxOpf>
// ─────────────────────────────────────────────────────────────────────────────

/// Build BusOpf objects by merging network data with OPF results.
pub fn buses_opf(opf: &CoreOpfSolution, net: &CoreNetwork) -> Vec<BusOpf> {
    let pf = &opf.power_flow;
    let load_p = net.bus_load_p_mw();
    let load_q = net.bus_load_q_mvar();
    let bus_ext_to_idx: std::collections::HashMap<u32, usize> = pf
        .bus_numbers
        .iter()
        .enumerate()
        .map(|(i, &n)| (n, i))
        .collect();

    net.buses
        .iter()
        .enumerate()
        .map(|(bus_idx, b)| {
            let idx = bus_ext_to_idx.get(&b.number).copied();
            let i = idx.unwrap_or(0);
            let vm = pf
                .voltage_magnitude_pu
                .get(i)
                .copied()
                .unwrap_or(b.voltage_magnitude_pu);
            let va = pf
                .voltage_angle_rad
                .get(i)
                .copied()
                .unwrap_or(b.voltage_angle_rad)
                .to_degrees();
            let lmp = opf.pricing.lmp.get(i).copied().unwrap_or(0.0);
            let lmp_energy = opf.pricing.lmp_energy.get(i).copied().unwrap_or(0.0);
            let lmp_congestion = opf.pricing.lmp_congestion.get(i).copied().unwrap_or(0.0);
            let lmp_loss = opf.pricing.lmp_loss.get(i).copied().unwrap_or(0.0);
            let lmp_reactive = opf.pricing.lmp_reactive.get(i).copied().unwrap_or(0.0);
            let mu_vmin = opf
                .branches
                .shadow_price_vm_min
                .get(i)
                .copied()
                .unwrap_or(0.0);
            let mu_vmax = opf
                .branches
                .shadow_price_vm_max
                .get(i)
                .copied()
                .unwrap_or(0.0);
            BusOpf {
                number: b.number,
                name: b.name.clone(),
                type_str: bus_type_str(b.bus_type),
                pd_mw: load_p.get(bus_idx).copied().unwrap_or(0.0),
                qd_mvar: load_q.get(bus_idx).copied().unwrap_or(0.0),
                area: b.area,
                zone: b.zone,
                base_kv: b.base_kv,
                vmin_pu: b.voltage_min_pu,
                vmax_pu: b.voltage_max_pu,
                vm_pu: vm,
                va_deg: va,
                lmp,
                lmp_energy,
                lmp_congestion,
                lmp_loss,
                lmp_reactive,
                mu_vmin,
                mu_vmax,
            }
        })
        .collect()
}

/// Build BranchOpf objects from OPF results.
pub fn branches_opf(opf: &CoreOpfSolution, net: &CoreNetwork) -> Vec<BranchOpf> {
    use surge_network::network::TransformerConnection;
    net.branches
        .iter()
        .enumerate()
        .map(|(i, br)| {
            let pf_mw = opf
                .power_flow
                .branch_p_from_mw
                .get(i)
                .copied()
                .unwrap_or(0.0);
            let qf_mvar = opf
                .power_flow
                .branch_q_from_mvar
                .get(i)
                .copied()
                .unwrap_or(0.0);
            let pt_mw = opf
                .power_flow
                .branch_p_to_mw
                .get(i)
                .copied()
                .unwrap_or(-pf_mw);
            let qt_mvar = opf
                .power_flow
                .branch_q_to_mvar
                .get(i)
                .copied()
                .unwrap_or(0.0);
            let loading_pct = opf
                .branches
                .branch_loading_pct
                .get(i)
                .copied()
                .unwrap_or(0.0);
            let shadow_price = opf
                .branches
                .branch_shadow_prices
                .get(i)
                .copied()
                .unwrap_or(0.0);
            let mu_angmin = opf
                .branches
                .shadow_price_angmin
                .get(i)
                .copied()
                .unwrap_or(0.0);
            let mu_angmax = opf
                .branches
                .shadow_price_angmax
                .get(i)
                .copied()
                .unwrap_or(0.0);
            let losses_mw = pf_mw + pt_mw;
            BranchOpf {
                from_bus: br.from_bus,
                to_bus: br.to_bus,
                circuit: br.circuit.clone(),
                r_pu: br.r,
                x_pu: br.x,
                b_pu: br.b,
                rate_a_mva: br.rating_a_mva,
                rate_b_mva: br.rating_b_mva,
                rate_c_mva: br.rating_c_mva,
                tap: br.tap,
                shift_deg: br.phase_shift_rad.to_degrees(),
                in_service: br.in_service,
                g_mag: br.g_mag,
                b_mag: br.b_mag,
                transformer_connection: match br
                    .transformer_data
                    .as_ref()
                    .map(|t| t.transformer_connection)
                    .unwrap_or_default()
                {
                    TransformerConnection::WyeGWyeG => "WyeG-WyeG".to_string(),
                    TransformerConnection::WyeGDelta => "WyeG-Delta".to_string(),
                    TransformerConnection::DeltaWyeG => "Delta-WyeG".to_string(),
                    TransformerConnection::DeltaDelta => "Delta-Delta".to_string(),
                    TransformerConnection::WyeGWye => "WyeG-Wye".to_string(),
                },
                angmin_deg: br.angle_diff_min_rad.map(|a| a.to_degrees()),
                angmax_deg: br.angle_diff_max_rad.map(|a| a.to_degrees()),
                pf_mw,
                qf_mvar,
                pt_mw,
                qt_mvar,
                loading_pct,
                losses_mw,
                shadow_price,
                mu_angmin,
                mu_angmax,
            }
        })
        .collect()
}

/// Build GenOpf objects from OPF results.
///
/// `opf.generators.gen_p_mw` is indexed by **in-service** generators only.
/// We match by the canonical generator IDs carried by the OPF solution.
pub fn generators_opf(opf: &CoreOpfSolution, net: &CoreNetwork) -> Vec<GenOpf> {
    let gen_lookup: std::collections::HashMap<String, usize> = opf
        .generators
        .gen_ids
        .iter()
        .cloned()
        .enumerate()
        .map(|(idx, generator_id)| (generator_id, idx))
        .collect();
    net.generators
        .iter()
        .map(|g| {
            let machine_id = g.machine_id.clone().unwrap_or_else(|| "1".to_string());
            let opf_idx = if g.in_service {
                gen_lookup.get(&g.id).copied()
            } else {
                None
            };
            let (pg, qg, mu_pmin, mu_pmax, mu_qmin, mu_qmax) = if let Some(opf_idx) = opf_idx {
                let pg = opf.generators.gen_p_mw.get(opf_idx).copied().unwrap_or(g.p);
                let qg = opf
                    .generators
                    .gen_q_mvar
                    .get(opf_idx)
                    .copied()
                    .unwrap_or(0.0);
                let mu_pmin = opf
                    .generators
                    .shadow_price_pg_min
                    .get(opf_idx)
                    .copied()
                    .unwrap_or(0.0);
                let mu_pmax = opf
                    .generators
                    .shadow_price_pg_max
                    .get(opf_idx)
                    .copied()
                    .unwrap_or(0.0);
                let mu_qmin = opf
                    .generators
                    .shadow_price_qg_min
                    .get(opf_idx)
                    .copied()
                    .unwrap_or(0.0);
                let mu_qmax = opf
                    .generators
                    .shadow_price_qg_max
                    .get(opf_idx)
                    .copied()
                    .unwrap_or(0.0);
                (pg, qg, mu_pmin, mu_pmax, mu_qmin, mu_qmax)
            } else {
                (g.p, g.q, 0.0, 0.0, 0.0, 0.0)
            };
            let (
                cost_model,
                cost_startup,
                cost_shutdown,
                cost_coefficients,
                cost_breakpoints_mw,
                cost_breakpoints_usd,
            ) = flatten_cost_curve(&g.cost);
            GenOpf {
                bus: g.bus,
                machine_id,
                pmax_mw: g.pmax,
                pmin_mw: g.pmin,
                qmax_mvar: g.qmax,
                qmin_mvar: g.qmin,
                vs_pu: g.voltage_setpoint_pu,
                mbase_mva: g.machine_base_mva,
                co2_rate_t_per_mwh: g.fuel.as_ref().map(|f| f.emission_rates.co2).unwrap_or(0.0),
                in_service: g.in_service,
                fuel_type: g.fuel.as_ref().and_then(|f| f.fuel_type.clone()),
                p_mw: pg,
                q_mvar: qg,
                mu_pmin,
                mu_pmax,
                mu_qmin,
                mu_qmax,
                cost_model,
                cost_startup,
                cost_shutdown,
                cost_coefficients,
                cost_breakpoints_mw,
                cost_breakpoints_usd,
            }
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

fn commitment_status_str(cs: surge_network::network::CommitmentStatus) -> String {
    use surge_network::network::CommitmentStatus;
    match cs {
        CommitmentStatus::Market => "Market".to_string(),
        CommitmentStatus::SelfCommitted => "SelfCommitted".to_string(),
        CommitmentStatus::MustRun => "MustRun".to_string(),
        CommitmentStatus::Unavailable => "Unavailable".to_string(),
        CommitmentStatus::EmergencyOnly => "EmergencyOnly".to_string(),
    }
}

fn bus_type_str(bt: BusType) -> String {
    match bt {
        BusType::PQ => "PQ".to_string(),
        BusType::PV => "PV".to_string(),
        BusType::Slack => "Slack".to_string(),
        BusType::Isolated => "Isolated".to_string(),
    }
}

fn flatten_cost_curve(
    cost: &Option<CostCurve>,
) -> (Option<String>, f64, f64, Vec<f64>, Vec<f64>, Vec<f64>) {
    match cost {
        None => (None, 0.0, 0.0, vec![], vec![], vec![]),
        Some(CostCurve::Polynomial {
            startup,
            shutdown,
            coeffs,
        }) => (
            Some("polynomial".to_string()),
            *startup,
            *shutdown,
            coeffs.clone(),
            vec![],
            vec![],
        ),
        Some(CostCurve::PiecewiseLinear {
            startup,
            shutdown,
            points,
        }) => (
            Some("piecewise_linear".to_string()),
            *startup,
            *shutdown,
            vec![],
            points.iter().map(|&(p, _)| p).collect(),
            points.iter().map(|&(_, c)| c).collect(),
        ),
    }
}

fn eval_polynomial(coeffs: &[f64], p: f64) -> f64 {
    let mut result = 0.0;
    for &c in coeffs {
        result = result * p + c;
    }
    result
}

fn marginal_polynomial(coeffs: &[f64], p: f64) -> f64 {
    if coeffs.len() <= 1 {
        return 0.0;
    }
    let n = coeffs.len();
    let mut result = 0.0;
    for (i, &c) in coeffs[..n - 1].iter().enumerate() {
        let power = (n - 1 - i) as f64;
        result = result * p + power * c;
    }
    result
}

fn eval_piecewise(points: &[(f64, f64)], p: f64) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    if points.len() == 1 {
        return points[0].1;
    }
    if p <= points[0].0 {
        return points[0].1;
    }
    if p >= points[points.len() - 1].0 {
        return points[points.len() - 1].1;
    }
    for i in 1..points.len() {
        if p <= points[i].0 {
            let (x0, y0) = points[i - 1];
            let (x1, y1) = points[i];
            let dx = x1 - x0;
            if dx.abs() < 1e-20 {
                return y0;
            }
            return y0 + (y1 - y0) * (p - x0) / dx;
        }
    }
    points[points.len() - 1].1
}

fn marginal_piecewise(points: &[(f64, f64)], p: f64) -> f64 {
    if points.len() < 2 {
        return 0.0;
    }
    let slope = |i: usize| {
        let (x0, y0) = points[i - 1];
        let (x1, y1) = points[i];
        let dx = x1 - x0;
        if dx.abs() < 1e-20 {
            0.0
        } else {
            (y1 - y0) / dx
        }
    };
    if p <= points[0].0 {
        return slope(1);
    }
    if p >= points[points.len() - 1].0 {
        return slope(points.len() - 1);
    }
    for (i, pt) in points.iter().enumerate().skip(1) {
        if p <= pt.0 {
            return slope(i);
        }
    }
    0.0
}

/// Compute to-end power flows using the full π-model admittance formulation.
///
/// Returns Vec<(pt_mw, qt_mvar)> for each branch (positive = injection at to-bus into line).
fn compute_to_end_flows(pf: &CorePfSolution, net: &CoreNetwork) -> Vec<(f64, f64)> {
    let base = net.base_mva;
    let bus_map = net.bus_index_map();
    let mut flows = Vec::with_capacity(net.branches.len());

    for br in &net.branches {
        if !br.in_service {
            flows.push((0.0, 0.0));
            continue;
        }
        let f = match bus_map.get(&br.from_bus) {
            Some(&i) => i,
            None => {
                flows.push((0.0, 0.0));
                continue;
            }
        };
        let t = match bus_map.get(&br.to_bus) {
            Some(&i) => i,
            None => {
                flows.push((0.0, 0.0));
                continue;
            }
        };

        let vi = pf.voltage_magnitude_pu[f];
        let vj = pf.voltage_magnitude_pu[t];
        let theta_ij = pf.voltage_angle_rad[f] - pf.voltage_angle_rad[t];
        let branch_flows = br.power_flows_pu(vi, vj, theta_ij, 1e-40);

        flows.push((branch_flows.p_to_pu * base, branch_flows.q_to_pu * base));
    }
    flows
}

// ─────────────────────────────────────────────────────────────────────────────
// LccHvdcLink — LCC-HVDC two-terminal line
// ─────────────────────────────────────────────────────────────────────────────

/// LCC-HVDC (thyristor) two-terminal DC line.
///
/// Obtain via `net.hvdc.links`.
#[pyclass(name = "LccHvdcLink", skip_from_py_object)]
#[derive(Clone)]
pub struct DcLine {
    /// DC link name.
    #[pyo3(get)]
    pub name: String,
    /// Scheduled DC power (MW) for power-control mode.
    #[pyo3(get, set)]
    pub scheduled_setpoint: f64,
    /// Scheduled DC voltage (kV).
    #[pyo3(get, set)]
    pub scheduled_voltage_kv: f64,
    /// DC circuit resistance (ohms).
    #[pyo3(get, set)]
    pub resistance_ohm: f64,
    /// Rectifier AC bus number (AC → DC).
    #[pyo3(get, set)]
    pub rectifier_bus: u32,
    /// Inverter AC bus number (DC → AC).
    #[pyo3(get, set)]
    pub inverter_bus: u32,
    /// True if the link is in service (mode != Blocked).
    #[pyo3(get, set)]
    pub in_service: bool,
    /// Minimum DC power for joint AC-DC OPF (MW). When `p_dc_min_mw <
    /// p_dc_max_mw` the link's P becomes an NLP decision variable.
    #[pyo3(get, set)]
    pub p_dc_min_mw: f64,
    /// Maximum DC power for joint AC-DC OPF (MW).
    #[pyo3(get, set)]
    pub p_dc_max_mw: f64,
}

impl DcLine {
    pub fn from_core(dc: &CoreDcLine) -> Self {
        use surge_network::network::LccHvdcControlMode;
        Self {
            name: dc.name.clone(),
            scheduled_setpoint: dc.scheduled_setpoint,
            scheduled_voltage_kv: dc.scheduled_voltage_kv,
            resistance_ohm: dc.resistance_ohm,
            rectifier_bus: dc.rectifier.bus,
            inverter_bus: dc.inverter.bus,
            in_service: !matches!(dc.mode, LccHvdcControlMode::Blocked),
            p_dc_min_mw: dc.p_dc_min_mw,
            p_dc_max_mw: dc.p_dc_max_mw,
        }
    }
}

#[pymethods]
impl DcLine {
    #[new]
    #[pyo3(signature = (name, rectifier_bus, inverter_bus, scheduled_setpoint=0.0, scheduled_voltage_kv=500.0, resistance_ohm=0.0, in_service=true, p_dc_min_mw=0.0, p_dc_max_mw=0.0))]
    fn new(
        name: &str,
        rectifier_bus: u32,
        inverter_bus: u32,
        scheduled_setpoint: f64,
        scheduled_voltage_kv: f64,
        resistance_ohm: f64,
        in_service: bool,
        p_dc_min_mw: f64,
        p_dc_max_mw: f64,
    ) -> Self {
        Self {
            name: name.to_string(),
            scheduled_setpoint,
            scheduled_voltage_kv,
            resistance_ohm,
            rectifier_bus,
            inverter_bus,
            in_service,
            p_dc_min_mw,
            p_dc_max_mw,
        }
    }

    /// Scheduled DC power (MW). Alias for setvl.
    #[getter]
    fn p_mw(&self) -> f64 {
        self.scheduled_setpoint
    }

    /// True when the joint AC-DC OPF should treat this link's DC power
    /// as an NLP decision variable (`p_dc_min_mw < p_dc_max_mw`).
    #[getter]
    fn has_variable_p_dc(&self) -> bool {
        self.p_dc_min_mw < self.p_dc_max_mw
    }

    fn __repr__(&self) -> String {
        let p_range = if self.p_dc_min_mw < self.p_dc_max_mw {
            format!(" P∈[{:.1},{:.1}]", self.p_dc_min_mw, self.p_dc_max_mw)
        } else {
            String::new()
        };
        format!(
            "<LccHvdcLink '{}' rect={} inv={} P={:.1}MW{} Vdc={:.0}kV{}>",
            self.name,
            self.rectifier_bus,
            self.inverter_bus,
            self.scheduled_setpoint,
            p_range,
            self.scheduled_voltage_kv,
            if self.in_service { "" } else { " BLOCKED" }
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// VscHvdcLink — VSC-HVDC two-terminal line
// ─────────────────────────────────────────────────────────────────────────────

/// VSC-HVDC two-terminal DC line.
///
/// Obtain via `net.hvdc.links`.
#[pyclass(name = "VscHvdcLink", skip_from_py_object)]
#[derive(Clone)]
pub struct VscDcLine {
    #[pyo3(get)]
    pub name: String,
    /// Active power setpoint (MW). Positive = bus1 sends power to bus2.
    #[pyo3(get, set)]
    pub p_mw: f64,
    /// Constant losses (MW).
    #[pyo3(get, set)]
    pub loss_a_mw: f64,
    /// Variable loss coefficient (MW/MW).
    #[pyo3(get, set)]
    pub loss_linear: f64,
    /// DC cable resistance (ohms).
    #[pyo3(get, set)]
    pub resistance_ohm: f64,
    /// Converter 1 AC bus number.
    #[pyo3(get, set)]
    pub converter1_bus: u32,
    /// Converter 2 AC bus number.
    #[pyo3(get, set)]
    pub converter2_bus: u32,
    /// True if in service (mode != Blocked).
    #[pyo3(get, set)]
    pub in_service: bool,
    /// Minimum reactive power at converter 1 (MVAr).
    #[pyo3(get, set)]
    pub q1_min_mvar: f64,
    /// Maximum reactive power at converter 1 (MVAr).
    #[pyo3(get, set)]
    pub q1_max_mvar: f64,
    /// Minimum reactive power at converter 2 (MVAr).
    #[pyo3(get, set)]
    pub q2_min_mvar: f64,
    /// Maximum reactive power at converter 2 (MVAr).
    #[pyo3(get, set)]
    pub q2_max_mvar: f64,
}

impl VscDcLine {
    pub fn from_core(vsc: &CoreVscDcLine) -> Self {
        use surge_network::network::VscHvdcControlMode;
        Self {
            name: vsc.name.clone(),
            p_mw: vsc.converter1.dc_setpoint,
            loss_a_mw: vsc.converter1.loss_constant_mw + vsc.converter2.loss_constant_mw,
            loss_linear: vsc.converter1.loss_linear + vsc.converter2.loss_linear,
            resistance_ohm: vsc.resistance_ohm,
            converter1_bus: vsc.converter1.bus,
            converter2_bus: vsc.converter2.bus,
            in_service: !matches!(vsc.mode, VscHvdcControlMode::Blocked),
            q1_min_mvar: vsc.converter1.q_min_mvar,
            q1_max_mvar: vsc.converter1.q_max_mvar,
            q2_min_mvar: vsc.converter2.q_min_mvar,
            q2_max_mvar: vsc.converter2.q_max_mvar,
        }
    }
}

#[pymethods]
impl VscDcLine {
    #[new]
    #[pyo3(signature = (name, converter1_bus, converter2_bus, p_mw=0.0, loss_a_mw=0.0, loss_linear=0.0, resistance_ohm=0.0, in_service=true, q1_min_mvar=-9999.0, q1_max_mvar=9999.0, q2_min_mvar=-9999.0, q2_max_mvar=9999.0))]
    fn new(
        name: &str,
        converter1_bus: u32,
        converter2_bus: u32,
        p_mw: f64,
        loss_a_mw: f64,
        loss_linear: f64,
        resistance_ohm: f64,
        in_service: bool,
        q1_min_mvar: f64,
        q1_max_mvar: f64,
        q2_min_mvar: f64,
        q2_max_mvar: f64,
    ) -> Self {
        Self {
            name: name.to_string(),
            p_mw,
            loss_a_mw,
            loss_linear,
            resistance_ohm,
            converter1_bus,
            converter2_bus,
            in_service,
            q1_min_mvar,
            q1_max_mvar,
            q2_min_mvar,
            q2_max_mvar,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "<VscHvdcLink '{}' bus{}↔{} P={:.1}MW{}>",
            self.name,
            self.converter1_bus,
            self.converter2_bus,
            self.p_mw,
            if self.in_service { "" } else { " BLOCKED" }
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DcBus / DcBranch / DcConverter — explicit DC-network topology
// ─────────────────────────────────────────────────────────────────────────────

/// An explicit DC bus in a canonical DC grid.
///
/// Obtain via `net.hvdc.dc_grids[i].buses`.
#[pyclass(name = "DcBus", skip_from_py_object)]
#[derive(Clone)]
pub struct DcBus {
    #[pyo3(get, set)]
    pub dc_bus: u32,
    #[pyo3(get, set)]
    pub p_dc_mw: f64,
    #[pyo3(get, set)]
    pub v_dc_pu: f64,
    #[pyo3(get, set)]
    pub base_kv_dc: f64,
    #[pyo3(get, set)]
    pub v_dc_min_pu: f64,
    #[pyo3(get, set)]
    pub v_dc_max_pu: f64,
    #[pyo3(get, set)]
    pub g_shunt_siemens: f64,
    #[pyo3(get, set)]
    pub r_ground_ohm: f64,
}

impl DcBus {
    pub fn from_core(bus: &CoreDcBus) -> Self {
        Self {
            dc_bus: bus.bus_id,
            p_dc_mw: bus.p_dc_mw,
            v_dc_pu: bus.v_dc_pu,
            base_kv_dc: bus.base_kv_dc,
            v_dc_min_pu: bus.v_dc_min,
            v_dc_max_pu: bus.v_dc_max,
            g_shunt_siemens: bus.g_shunt_siemens,
            r_ground_ohm: bus.r_ground_ohm,
        }
    }
}

#[pymethods]
impl DcBus {
    fn __repr__(&self) -> String {
        format!(
            "<DcBus {} Vdc={:.4}pu base={:.1}kV>",
            self.dc_bus, self.v_dc_pu, self.base_kv_dc
        )
    }
}

/// An explicit DC branch in a canonical DC grid.
///
/// Obtain via `net.hvdc.dc_grids[i].branches`.
#[pyclass(name = "DcBranch", skip_from_py_object)]
#[derive(Clone)]
pub struct DcBranch {
    #[pyo3(get, set)]
    pub from_bus: u32,
    #[pyo3(get, set)]
    pub to_bus: u32,
    #[pyo3(get, set)]
    pub resistance_ohm: f64,
    #[pyo3(get, set)]
    pub rating_a_mw: f64,
    #[pyo3(get, set)]
    pub rating_b_mw: f64,
    #[pyo3(get, set)]
    pub rating_c_mw: f64,
    #[pyo3(get, set)]
    pub in_service: bool,
}

impl DcBranch {
    pub fn from_core(branch: &CoreDcBranch) -> Self {
        Self {
            from_bus: branch.from_bus,
            to_bus: branch.to_bus,
            resistance_ohm: branch.r_ohm,
            rating_a_mw: branch.rating_a_mva,
            rating_b_mw: branch.rating_b_mva,
            rating_c_mw: branch.rating_c_mva,
            in_service: branch.status,
        }
    }
}

#[pymethods]
impl DcBranch {
    fn __repr__(&self) -> String {
        format!(
            "<DcBranch {}->{} R={:.4}ohm{}>",
            self.from_bus,
            self.to_bus,
            self.resistance_ohm,
            if self.in_service { "" } else { " OUT" }
        )
    }
}

/// An explicit AC/DC converter station in a canonical DC grid.
///
/// Obtain via `net.hvdc.dc_grids[i].converters`.
#[pyclass(name = "DcConverter", skip_from_py_object)]
#[derive(Clone)]
pub struct DcConverter {
    #[pyo3(get, set)]
    pub dc_bus: u32,
    #[pyo3(get, set)]
    pub ac_bus: u32,
    #[pyo3(get, set)]
    pub technology: String,
    #[pyo3(get, set)]
    pub dc_control_mode: String,
    #[pyo3(get, set)]
    pub ac_control_mode: String,
    #[pyo3(get, set)]
    pub power_dc_setpoint_mw: f64,
    #[pyo3(get, set)]
    pub reactive_power_mvar: f64,
    #[pyo3(get, set)]
    pub voltage_dc_setpoint_pu: f64,
    #[pyo3(get, set)]
    pub voltage_setpoint_pu: f64,
    #[pyo3(get, set)]
    pub droop_mw_per_pu: f64,
    #[pyo3(get, set)]
    pub loss_constant_mw: f64,
    #[pyo3(get, set)]
    pub loss_linear: f64,
    #[pyo3(get, set)]
    pub in_service: bool,
}

impl DcConverter {
    pub fn from_core(converter: &CoreDcConverter) -> Self {
        match converter {
            CoreDcConverter::Lcc(converter) => Self {
                dc_bus: converter.dc_bus,
                ac_bus: converter.ac_bus,
                technology: "lcc".to_string(),
                dc_control_mode: "power".to_string(),
                ac_control_mode: "lcc".to_string(),
                power_dc_setpoint_mw: converter.scheduled_setpoint,
                reactive_power_mvar: 0.0,
                voltage_dc_setpoint_pu: 1.0,
                voltage_setpoint_pu: 1.0,
                droop_mw_per_pu: 0.0,
                loss_constant_mw: 0.0,
                loss_linear: 0.0,
                in_service: converter.in_service,
            },
            CoreDcConverter::Vsc(converter) => {
                let dc_control_mode = match converter.control_type_dc {
                    2 => "voltage",
                    3 => "droop",
                    _ => "power",
                };
                let ac_control_mode = match converter.control_type_ac {
                    2 => "pv",
                    _ => "pq",
                };
                Self {
                    dc_bus: converter.dc_bus,
                    ac_bus: converter.ac_bus,
                    technology: "vsc".to_string(),
                    dc_control_mode: dc_control_mode.to_string(),
                    ac_control_mode: ac_control_mode.to_string(),
                    power_dc_setpoint_mw: converter.power_dc_setpoint_mw,
                    reactive_power_mvar: converter.reactive_power_mvar,
                    voltage_dc_setpoint_pu: converter.voltage_dc_setpoint_pu,
                    voltage_setpoint_pu: converter.voltage_setpoint_pu,
                    droop_mw_per_pu: converter.droop,
                    loss_constant_mw: converter.loss_constant_mw,
                    loss_linear: converter.loss_linear,
                    in_service: converter.status,
                }
            }
        }
    }
}

/// A canonical explicit DC grid.
///
/// Obtain via `net.hvdc.dc_grids`.
#[pyclass(name = "DcGrid", skip_from_py_object)]
#[derive(Clone)]
pub struct DcGrid {
    #[pyo3(get)]
    pub grid_id: u32,
    #[pyo3(get)]
    pub name: Option<String>,
    #[pyo3(get)]
    pub buses: Vec<DcBus>,
    #[pyo3(get)]
    pub branches: Vec<DcBranch>,
    #[pyo3(get)]
    pub converters: Vec<DcConverter>,
}

impl DcGrid {
    pub fn from_core(grid: &CoreDcGrid) -> Self {
        Self {
            grid_id: grid.id,
            name: grid.name.clone(),
            buses: grid.buses.iter().map(DcBus::from_core).collect(),
            branches: grid.branches.iter().map(DcBranch::from_core).collect(),
            converters: grid.converters.iter().map(DcConverter::from_core).collect(),
        }
    }
}

#[pymethods]
impl DcGrid {
    fn __repr__(&self) -> String {
        format!(
            "<DcGrid id={} buses={} branches={} converters={}>",
            self.grid_id,
            self.buses.len(),
            self.branches.len(),
            self.converters.len()
        )
    }
}

#[pymethods]
impl DcConverter {
    fn __repr__(&self) -> String {
        format!(
            "<DcConverter technology='{}' ac_bus={} dc_bus={} mode={}{}>",
            self.technology,
            self.ac_bus,
            self.dc_bus,
            self.dc_control_mode,
            if self.in_service { "" } else { " OUT" }
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DispatchableLoad — demand-response resource
// ─────────────────────────────────────────────────────────────────────────────

/// A dispatchable (demand-response) load resource.
///
/// Obtain via `net.dispatchable_loads`.
#[pyclass(name = "DispatchableLoad", skip_from_py_object)]
#[derive(Clone)]
pub struct DispatchableLoad {
    /// Stable index in ``network.dispatchable_loads``.
    #[pyo3(get)]
    pub index: Option<usize>,
    /// External bus number where this load is connected.
    #[pyo3(get, set)]
    pub bus: u32,
    /// Scheduled real power consumption (MW). Positive = consuming.
    #[pyo3(get, set)]
    pub p_sched_mw: f64,
    /// Scheduled reactive power consumption (MVAr).
    #[pyo3(get, set)]
    pub q_sched_mvar: f64,
    /// Minimum real power served (MW).
    #[pyo3(get, set)]
    pub pmin_mw: f64,
    /// Maximum real power served (MW).
    #[pyo3(get, set)]
    pub pmax_mw: f64,
    /// Minimum reactive power served (MVAr).
    #[pyo3(get, set)]
    pub qmin_mvar: f64,
    /// Maximum reactive power served (MVAr).
    #[pyo3(get, set)]
    pub qmax_mvar: f64,
    /// Demand-response archetype used by the public edit APIs.
    ///
    /// Python creation/update currently supports ``"Curtailable"`` and
    /// ``"Interruptible"``. Imported networks may still surface additional
    /// archetype strings when reading existing model data.
    #[pyo3(get, set)]
    pub archetype: String,
    /// True if load has a fixed power factor (Q tracks P proportionally).
    #[pyo3(get, set)]
    pub fixed_power_factor: bool,
    /// True if in service for OPF.
    #[pyo3(get, set)]
    pub in_service: bool,
    /// Market product type (e.g., "ECRSS", "ERS"). None if unspecified.
    #[pyo3(get, set)]
    pub product_type: Option<String>,
    /// Customer baseline load (MW) for settlement curtailment measurement.
    #[pyo3(get, set)]
    pub baseline_mw: Option<f64>,
    /// Linear curtailment / interruption cost ($/MWh) when applicable.
    #[pyo3(get, set)]
    pub cost_per_mwh: Option<f64>,
    /// Reserve offers: list of (product_id, capacity_mw, cost_per_mwh).
    #[pyo3(get, set)]
    pub reserve_offers: Vec<(String, f64, f64)>,
    /// Reserve qualification flags: {product_id: qualified}.
    #[pyo3(get, set)]
    pub qualifications: std::collections::HashMap<String, bool>,
}

impl DispatchableLoad {
    pub fn from_core(
        index: usize,
        dl: &CoreDispatchableLoad,
        _buses: &[surge_network::network::Bus],
        base_mva: f64,
    ) -> Self {
        use surge_network::market::LoadArchetype;
        let archetype = match dl.archetype {
            LoadArchetype::Curtailable => "Curtailable",
            LoadArchetype::Elastic => "Elastic",
            LoadArchetype::Interruptible => "Interruptible",
            LoadArchetype::IndependentPQ => "IndependentPQ",
        }
        .to_string();
        Self {
            index: Some(index),
            bus: dl.bus,
            p_sched_mw: dl.p_sched_pu * base_mva,
            q_sched_mvar: dl.q_sched_pu * base_mva,
            pmin_mw: dl.p_min_pu * base_mva,
            pmax_mw: dl.p_max_pu * base_mva,
            qmin_mvar: dl.q_min_pu * base_mva,
            qmax_mvar: dl.q_max_pu * base_mva,
            archetype,
            fixed_power_factor: dl.fixed_power_factor,
            in_service: dl.in_service,
            product_type: dl.product_type.clone(),
            baseline_mw: dl.baseline_mw,
            cost_per_mwh: match &dl.cost_model {
                surge_network::market::LoadCostModel::LinearCurtailment { cost_per_mw }
                | surge_network::market::LoadCostModel::InterruptPenalty { cost_per_mw } => {
                    Some(*cost_per_mw)
                }
                _ => None,
            },
            reserve_offers: dl
                .reserve_offers
                .iter()
                .map(|offer| {
                    (
                        offer.product_id.clone(),
                        offer.capacity_mw,
                        offer.cost_per_mwh,
                    )
                })
                .collect(),
            qualifications: dl.qualifications.clone(),
        }
    }
}

#[pymethods]
impl DispatchableLoad {
    #[new]
    #[pyo3(signature = (bus, archetype="Curtailable", p_sched_mw=0.0, q_sched_mvar=0.0, pmin_mw=0.0, pmax_mw=0.0, qmin_mvar=0.0, qmax_mvar=0.0, fixed_power_factor=true, in_service=true, product_type=None, baseline_mw=None, cost_per_mwh=None, reserve_offers=None, qualifications=None))]
    fn new(
        bus: u32,
        archetype: &str,
        p_sched_mw: f64,
        q_sched_mvar: f64,
        pmin_mw: f64,
        pmax_mw: f64,
        qmin_mvar: f64,
        qmax_mvar: f64,
        fixed_power_factor: bool,
        in_service: bool,
        product_type: Option<String>,
        baseline_mw: Option<f64>,
        cost_per_mwh: Option<f64>,
        reserve_offers: Option<Vec<(String, f64, f64)>>,
        qualifications: Option<std::collections::HashMap<String, bool>>,
    ) -> PyResult<Self> {
        if !matches!(archetype, "Curtailable" | "Interruptible") {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "dispatchable load archetype must be 'Curtailable' or 'Interruptible'; got '{archetype}'"
            )));
        }
        Ok(Self {
            index: None,
            bus,
            p_sched_mw,
            q_sched_mvar,
            pmin_mw,
            pmax_mw,
            qmin_mvar,
            qmax_mvar,
            archetype: archetype.to_string(),
            fixed_power_factor,
            in_service,
            product_type,
            baseline_mw,
            cost_per_mwh,
            reserve_offers: reserve_offers.unwrap_or_default(),
            qualifications: qualifications.unwrap_or_default(),
        })
    }

    /// True if this resource can also generate (pmin_mw < 0).
    #[getter]
    fn is_generator(&self) -> bool {
        self.pmin_mw < 0.0
    }

    fn __repr__(&self) -> String {
        format!(
            "<DispatchableLoad bus={} {} {:.1}/{:.1}MW{}>",
            self.bus,
            self.archetype,
            self.pmin_mw,
            self.pmax_mw,
            if self.in_service { "" } else { " OUT" }
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FactsDevice — SVC / STATCOM / TCSC / UPFC
// ─────────────────────────────────────────────────────────────────────────────

/// A FACTS control device (SVC, STATCOM, TCSC, UPFC).
///
/// Obtain via `net.facts_devices`.
#[pyclass(name = "FactsDevice", skip_from_py_object)]
#[derive(Clone)]
pub struct FactsDevice {
    #[pyo3(get)]
    pub name: String,
    /// Shunt connection bus number.
    #[pyo3(get, set)]
    pub bus_from: u32,
    /// Series/remote bus number (0 if shunt-only).
    #[pyo3(get, set)]
    pub bus_to: u32,
    /// Operating mode string.
    #[pyo3(get, set)]
    pub mode: String,
    /// Desired active power flow through series element (MW).
    #[pyo3(get, set)]
    pub p_des_mw: f64,
    /// Desired reactive power from shunt element (MVAr).
    #[pyo3(get, set)]
    pub q_des_mvar: f64,
    /// Voltage setpoint at bus_from (pu).
    #[pyo3(get, set)]
    pub v_set_pu: f64,
    /// Maximum shunt reactive injection (MVAr). Minimum is -q_max.
    #[pyo3(get, set)]
    pub q_max_mvar: f64,
    /// Series reactance contribution (pu, negative = impedance reduction).
    #[pyo3(get, set)]
    pub linx_pu: f64,
    #[pyo3(get, set)]
    pub in_service: bool,
}

impl FactsDevice {
    pub fn from_core(f: &CoreFactsDevice) -> Self {
        use surge_network::network::FactsMode;
        let mode = match f.mode {
            FactsMode::OutOfService => "OutOfService",
            FactsMode::SeriesOnly => "SeriesOnly",
            FactsMode::ShuntOnly => "ShuntOnly",
            FactsMode::ShuntSeries => "ShuntSeries",
            FactsMode::SeriesPowerControl => "SeriesPowerControl",
            FactsMode::ImpedanceModulation => "ImpedanceModulation",
        }
        .to_string();
        Self {
            name: f.name.clone(),
            bus_from: f.bus_from,
            bus_to: f.bus_to,
            mode,
            p_des_mw: f.p_setpoint_mw,
            q_des_mvar: f.q_setpoint_mvar,
            v_set_pu: f.voltage_setpoint_pu,
            q_max_mvar: f.q_max,
            linx_pu: f.series_reactance_pu,
            in_service: f.in_service,
        }
    }
}

#[pymethods]
impl FactsDevice {
    #[new]
    #[pyo3(signature = (name, bus_from, bus_to=0, mode="ShuntOnly", p_des_mw=0.0, q_des_mvar=0.0, v_set_pu=1.0, q_max_mvar=9999.0, linx_pu=0.0, in_service=true))]
    fn new(
        name: &str,
        bus_from: u32,
        bus_to: u32,
        mode: &str,
        p_des_mw: f64,
        q_des_mvar: f64,
        v_set_pu: f64,
        q_max_mvar: f64,
        linx_pu: f64,
        in_service: bool,
    ) -> Self {
        Self {
            name: name.to_string(),
            bus_from,
            bus_to,
            mode: mode.to_string(),
            p_des_mw,
            q_des_mvar,
            v_set_pu,
            q_max_mvar,
            linx_pu,
            in_service,
        }
    }

    /// True if this device has a shunt element.
    #[getter]
    fn has_shunt(&self) -> bool {
        matches!(
            self.mode.as_str(),
            "ShuntOnly" | "ShuntSeries" | "SeriesPowerControl"
        )
    }
    /// True if this device has a series element.
    #[getter]
    fn has_series(&self) -> bool {
        matches!(
            self.mode.as_str(),
            "SeriesOnly" | "ShuntSeries" | "SeriesPowerControl" | "ImpedanceModulation"
        )
    }

    fn __repr__(&self) -> String {
        format!(
            "<FactsDevice '{}' bus_from={} mode={}{}>",
            self.name,
            self.bus_from,
            self.mode,
            if self.bus_to > 0 {
                format!(" bus_to={}", self.bus_to)
            } else {
                String::new()
            }
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SwitchedShuntOpf — OPF-dispatched switched shunt
// ─────────────────────────────────────────────────────────────────────────────

/// A switched shunt with OPF dispatch result.
///
/// Obtain via `opf_result.switched_shunts(net)`.
#[pyclass(name = "SwitchedShuntOpf", skip_from_py_object)]
#[derive(Clone)]
pub struct SwitchedShuntOpf {
    /// External bus number.
    #[pyo3(get)]
    pub bus: u32,
    /// Minimum susceptance (pu).
    #[pyo3(get)]
    pub b_min_pu: f64,
    /// Maximum susceptance (pu).
    #[pyo3(get)]
    pub b_max_pu: f64,
    /// Dispatched susceptance — continuous NLP value (pu).
    #[pyo3(get)]
    pub b_dispatch_pu: f64,
    /// Dispatched susceptance — rounded to discrete steps (pu).
    #[pyo3(get)]
    pub b_rounded_pu: f64,
    /// Reactive injection at 1.0 pu voltage (MVAr) = b_dispatch_pu * base_mva.
    #[pyo3(get)]
    pub q_mvar: f64,
}

impl SwitchedShuntOpf {
    pub fn from_core(
        ss: &CoreSwitchedShuntOpf,
        b_dispatch: f64,
        b_rounded: f64,
        _buses: &[surge_network::network::Bus],
        base_mva: f64,
    ) -> Self {
        Self {
            bus: ss.bus,
            b_min_pu: ss.b_min_pu,
            b_max_pu: ss.b_max_pu,
            b_dispatch_pu: b_dispatch,
            b_rounded_pu: b_rounded,
            q_mvar: b_dispatch * base_mva,
        }
    }
}

#[pymethods]
impl SwitchedShuntOpf {
    fn __repr__(&self) -> String {
        format!(
            "<SwitchedShuntOpf bus={} B={:.4}pu ({:.1}MVAr)>",
            self.bus, self.b_dispatch_pu, self.q_mvar
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// AreaSchedule — Control area scheduled interchange
// ─────────────────────────────────────────────────────────────────────────────

/// A control area with scheduled power interchange.
///
/// Obtain via `net.area_schedules`.
#[pyclass(name = "AreaSchedule", skip_from_py_object)]
#[derive(Clone)]
pub struct AreaSchedule {
    /// Area number.
    #[pyo3(get)]
    pub area: u32,
    /// Area slack bus number.
    #[pyo3(get, set)]
    pub slack_bus: u32,
    /// Desired net real power export from area (MW).
    #[pyo3(get, set)]
    pub p_desired_mw: f64,
    /// Interchange tolerance (MW).
    #[pyo3(get, set)]
    pub p_tolerance_mw: f64,
    /// Area name.
    #[pyo3(get, set)]
    pub name: String,
}

impl AreaSchedule {
    pub fn from_core(a: &CoreAreaSchedule) -> Self {
        Self {
            area: a.number,
            slack_bus: a.slack_bus,
            p_desired_mw: a.p_desired_mw,
            p_tolerance_mw: a.p_tolerance_mw,
            name: a.name.clone(),
        }
    }
}

#[pymethods]
impl AreaSchedule {
    #[new]
    #[pyo3(signature = (area, slack_bus, p_desired_mw=0.0, p_tolerance_mw=10.0, name=""))]
    fn new(area: u32, slack_bus: u32, p_desired_mw: f64, p_tolerance_mw: f64, name: &str) -> Self {
        Self {
            area,
            slack_bus,
            p_desired_mw,
            p_tolerance_mw,
            name: name.to_string(),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "<AreaSchedule area={} '{}' Pdes={:.1}MW±{:.1}MW slack_bus={}>",
            self.area, self.name, self.p_desired_mw, self.p_tolerance_mw, self.slack_bus
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PumpedHydroUnit
// ─────────────────────────────────────────────────────────────────────────────

/// A pumped hydro storage unit (synchronous machine overlay).
///
/// Obtain via `net.pumped_hydro_units`.
#[pyclass(name = "PumpedHydroUnit", skip_from_py_object)]
#[derive(Clone)]
pub struct PumpedHydroUnit {
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub generator_bus: u32,
    #[pyo3(get)]
    pub generator_id: String,
    #[pyo3(get, set)]
    pub variable_speed: bool,
    #[pyo3(get, set)]
    pub pump_mw_fixed: f64,
    #[pyo3(get, set)]
    pub pump_mw_min: Option<f64>,
    #[pyo3(get, set)]
    pub pump_mw_max: Option<f64>,
    #[pyo3(get, set)]
    pub mode_transition_min: f64,
    #[pyo3(get, set)]
    pub condenser_capable: bool,
    #[pyo3(get, set)]
    pub upper_reservoir_mwh: f64,
    #[pyo3(get, set)]
    pub lower_reservoir_mwh: f64,
    #[pyo3(get, set)]
    pub soc_initial_mwh: f64,
    #[pyo3(get, set)]
    pub soc_min_mwh: f64,
    #[pyo3(get, set)]
    pub soc_max_mwh: f64,
    #[pyo3(get, set)]
    pub efficiency_generate: f64,
    #[pyo3(get, set)]
    pub efficiency_pump: f64,
    #[pyo3(get, set)]
    pub n_units: u32,
    #[pyo3(get, set)]
    pub shared_penstock_mw_max: Option<f64>,
    #[pyo3(get, set)]
    pub min_release_mw: f64,
    #[pyo3(get, set)]
    pub ramp_rate_mw_per_min: Option<f64>,
    #[pyo3(get, set)]
    pub startup_time_gen_min: f64,
    #[pyo3(get, set)]
    pub startup_time_pump_min: f64,
    #[pyo3(get, set)]
    pub startup_cost: f64,
    // Reserve offers: list of (product_id, capacity_mw, cost_per_mwh)
    #[pyo3(get, set)]
    pub reserve_offers: Vec<(String, f64, f64)>,
    // Reserve qualification flags: {product_id: qualified}
    #[pyo3(get, set)]
    pub qualifications: std::collections::HashMap<String, bool>,
}

impl PumpedHydroUnit {
    pub fn from_core(u: &CorePumpedHydroUnit) -> Self {
        Self {
            name: u.name.clone(),
            generator_bus: u.generator.bus,
            generator_id: u.generator.id.clone(),
            variable_speed: u.variable_speed,
            pump_mw_fixed: u.pump_mw_fixed,
            pump_mw_min: u.pump_mw_min,
            pump_mw_max: u.pump_mw_max,
            mode_transition_min: u.mode_transition_min,
            condenser_capable: u.condenser_capable,
            upper_reservoir_mwh: u.upper_reservoir_mwh,
            lower_reservoir_mwh: u.lower_reservoir_mwh,
            soc_initial_mwh: u.soc_initial_mwh,
            soc_min_mwh: u.soc_min_mwh,
            soc_max_mwh: u.soc_max_mwh,
            efficiency_generate: u.efficiency_generate,
            efficiency_pump: u.efficiency_pump,
            n_units: u.n_units,
            shared_penstock_mw_max: u.shared_penstock_mw_max,
            min_release_mw: u.min_release_mw,
            ramp_rate_mw_per_min: u.ramp_rate_mw_per_min,
            startup_time_gen_min: u.startup_time_gen_min,
            startup_time_pump_min: u.startup_time_pump_min,
            startup_cost: u.startup_cost,
            reserve_offers: u
                .reserve_offers
                .iter()
                .map(|o| (o.product_id.clone(), o.capacity_mw, o.cost_per_mwh))
                .collect(),
            qualifications: u.qualifications.clone(),
        }
    }
}

#[pymethods]
impl PumpedHydroUnit {
    #[new]
    #[pyo3(signature = (name, generator_bus, generator_id, capacity_mwh))]
    fn new(name: &str, generator_bus: u32, generator_id: &str, capacity_mwh: f64) -> Self {
        Self::from_core(&CorePumpedHydroUnit::new(
            name.to_string(),
            GeneratorRef {
                bus: generator_bus,
                id: generator_id.to_string(),
            },
            capacity_mwh,
        ))
    }

    fn __repr__(&self) -> String {
        format!(
            "<PumpedHydroUnit '{}' gen=({}, '{}') SoC=[{:.0},{:.0}]MWh eff_gen={:.0}% eff_pump={:.0}% units={}>",
            self.name,
            self.generator_bus,
            self.generator_id,
            self.soc_min_mwh,
            self.soc_max_mwh,
            self.efficiency_generate * 100.0,
            self.efficiency_pump * 100.0,
            self.n_units
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BreakerRating
// ─────────────────────────────────────────────────────────────────────────────

/// Circuit breaker rating at a bus.
///
/// Obtain via `net.breaker_ratings`.
#[pyclass(name = "BreakerRating", skip_from_py_object)]
#[derive(Clone)]
pub struct BreakerRating {
    #[pyo3(get)]
    pub bus: u32,
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get, set)]
    pub rated_kv: f64,
    #[pyo3(get, set)]
    pub interrupting_ka: f64,
    #[pyo3(get, set)]
    pub momentary_ka: Option<f64>,
    #[pyo3(get, set)]
    pub clearing_time_cycles: f64,
    #[pyo3(get, set)]
    pub in_service: bool,
}

impl BreakerRating {
    pub fn from_core(br: &CoreBreakerRating) -> Self {
        Self {
            bus: br.bus,
            name: br.name.clone(),
            rated_kv: br.rated_kv,
            interrupting_ka: br.interrupting_ka,
            momentary_ka: br.momentary_ka,
            clearing_time_cycles: br.clearing_time_cycles,
            in_service: br.in_service,
        }
    }
}

#[pymethods]
impl BreakerRating {
    #[new]
    #[pyo3(signature = (bus, name, rated_kv, interrupting_ka, momentary_ka=None, clearing_time_cycles=5.0, in_service=true))]
    fn new(
        bus: u32,
        name: &str,
        rated_kv: f64,
        interrupting_ka: f64,
        momentary_ka: Option<f64>,
        clearing_time_cycles: f64,
        in_service: bool,
    ) -> Self {
        Self {
            bus,
            name: name.to_string(),
            rated_kv,
            interrupting_ka,
            momentary_ka,
            clearing_time_cycles,
            in_service,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "<BreakerRating '{}' bus={} {:.1}kV int={:.1}kA>",
            self.name, self.bus, self.rated_kv, self.interrupting_ka
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FixedShunt
// ─────────────────────────────────────────────────────────────────────────────

/// A fixed shunt device at a bus.
///
/// Obtain via `net.fixed_shunts`.
#[pyclass(name = "FixedShunt", skip_from_py_object)]
#[derive(Clone)]
pub struct FixedShunt {
    #[pyo3(get)]
    pub bus: u32,
    #[pyo3(get)]
    pub id: String,
    /// "Capacitor", "Reactor", or "HarmonicFilter".
    #[pyo3(get, set)]
    pub shunt_type: String,
    #[pyo3(get, set)]
    pub g_mw: f64,
    #[pyo3(get, set)]
    pub b_mvar: f64,
    #[pyo3(get, set)]
    pub in_service: bool,
    #[pyo3(get, set)]
    pub rated_kv: Option<f64>,
    #[pyo3(get, set)]
    pub rated_mvar: Option<f64>,
}

impl FixedShunt {
    pub fn from_core(s: &CoreFixedShunt) -> Self {
        Self {
            bus: s.bus,
            id: s.id.clone(),
            shunt_type: match s.shunt_type {
                ShuntType::Capacitor => "Capacitor".to_string(),
                ShuntType::Reactor => "Reactor".to_string(),
                ShuntType::HarmonicFilter => "HarmonicFilter".to_string(),
            },
            g_mw: s.g_mw,
            b_mvar: s.b_mvar,
            in_service: s.in_service,
            rated_kv: s.rated_kv,
            rated_mvar: s.rated_mvar,
        }
    }
}

#[pymethods]
impl FixedShunt {
    #[new]
    #[pyo3(signature = (bus, id, shunt_type="Capacitor", g_mw=0.0, b_mvar=0.0, in_service=true, rated_kv=None, rated_mvar=None))]
    fn new(
        bus: u32,
        id: &str,
        shunt_type: &str,
        g_mw: f64,
        b_mvar: f64,
        in_service: bool,
        rated_kv: Option<f64>,
        rated_mvar: Option<f64>,
    ) -> Self {
        Self {
            bus,
            id: id.to_string(),
            shunt_type: shunt_type.to_string(),
            g_mw,
            b_mvar,
            in_service,
            rated_kv,
            rated_mvar,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "<FixedShunt bus={} id='{}' {} G={:.2}MW B={:.2}MVAr>",
            self.bus, self.id, self.shunt_type, self.g_mw, self.b_mvar
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CombinedCyclePlant (with nested Config + Transition helpers)
// ─────────────────────────────────────────────────────────────────────────────

/// A single combined cycle configuration (e.g. "1x0", "2x1").
#[pyclass(name = "CombinedCycleConfig", from_py_object)]
#[derive(Clone)]
pub struct CombinedCycleConfig {
    #[pyo3(get, set)]
    pub name: String,
    #[pyo3(get, set)]
    pub gen_indices: Vec<usize>,
    #[pyo3(get, set)]
    pub p_min_mw: f64,
    #[pyo3(get, set)]
    pub p_max_mw: f64,
    #[pyo3(get, set)]
    pub min_up_time_hr: f64,
    #[pyo3(get, set)]
    pub min_down_time_hr: f64,
}

impl CombinedCycleConfig {
    pub fn from_core(c: &CoreCombinedCycleConfig) -> Self {
        Self {
            name: c.name.clone(),
            gen_indices: c.gen_indices.clone(),
            p_min_mw: c.p_min_mw,
            p_max_mw: c.p_max_mw,
            min_up_time_hr: c.min_up_time_hr,
            min_down_time_hr: c.min_down_time_hr,
        }
    }
}

#[pymethods]
impl CombinedCycleConfig {
    #[new]
    #[pyo3(signature = (name, gen_indices, p_min_mw=0.0, p_max_mw=0.0, min_up_time_hr=0.0, min_down_time_hr=0.0))]
    fn new(
        name: &str,
        gen_indices: Vec<usize>,
        p_min_mw: f64,
        p_max_mw: f64,
        min_up_time_hr: f64,
        min_down_time_hr: f64,
    ) -> Self {
        Self {
            name: name.to_string(),
            gen_indices,
            p_min_mw,
            p_max_mw,
            min_up_time_hr,
            min_down_time_hr,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "<CombinedCycleConfig '{}' P=[{:.0},{:.0}]MW gens={:?}>",
            self.name, self.p_min_mw, self.p_max_mw, self.gen_indices
        )
    }
}

/// A transition between two combined cycle configurations.
#[pyclass(name = "CombinedCycleTransition", from_py_object)]
#[derive(Clone)]
pub struct CombinedCycleTransition {
    #[pyo3(get, set)]
    pub from_config: String,
    #[pyo3(get, set)]
    pub to_config: String,
    #[pyo3(get, set)]
    pub transition_time_min: f64,
    #[pyo3(get, set)]
    pub transition_cost: f64,
    #[pyo3(get, set)]
    pub online_transition: bool,
}

impl CombinedCycleTransition {
    pub fn from_core(t: &CoreCombinedCycleTransition) -> Self {
        Self {
            from_config: t.from_config.clone(),
            to_config: t.to_config.clone(),
            transition_time_min: t.transition_time_min,
            transition_cost: t.transition_cost,
            online_transition: t.online_transition,
        }
    }
}

#[pymethods]
impl CombinedCycleTransition {
    #[new]
    #[pyo3(signature = (from_config, to_config, transition_time_min=0.0, transition_cost=0.0, online_transition=false))]
    fn new(
        from_config: &str,
        to_config: &str,
        transition_time_min: f64,
        transition_cost: f64,
        online_transition: bool,
    ) -> Self {
        Self {
            from_config: from_config.to_string(),
            to_config: to_config.to_string(),
            transition_time_min,
            transition_cost,
            online_transition,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "<CombinedCycleTransition '{}' -> '{}' {:.0}min ${:.0}>",
            self.from_config, self.to_config, self.transition_time_min, self.transition_cost
        )
    }
}

/// A combined cycle power plant with multiple configurations.
///
/// Obtain via `net.combined_cycle_plants`.
#[pyclass(name = "CombinedCyclePlant", skip_from_py_object)]
#[derive(Clone)]
pub struct CombinedCyclePlant {
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get, set)]
    pub configs: Vec<CombinedCycleConfig>,
    #[pyo3(get, set)]
    pub transitions: Vec<CombinedCycleTransition>,
    #[pyo3(get, set)]
    pub active_config: Option<String>,
    #[pyo3(get, set)]
    pub hours_in_config: f64,
    #[pyo3(get, set)]
    pub duct_firing_capable: bool,
}

impl CombinedCyclePlant {
    pub fn from_core(p: &CoreCombinedCyclePlant) -> Self {
        Self {
            name: p.name.clone(),
            configs: p
                .configs
                .iter()
                .map(CombinedCycleConfig::from_core)
                .collect(),
            transitions: p
                .transitions
                .iter()
                .map(CombinedCycleTransition::from_core)
                .collect(),
            active_config: p.active_config.clone(),
            hours_in_config: p.hours_in_config,
            duct_firing_capable: p.duct_firing_capable,
        }
    }
}

#[pymethods]
impl CombinedCyclePlant {
    #[new]
    #[pyo3(signature = (name, configs=None, transitions=None, active_config=None, hours_in_config=0.0, duct_firing_capable=false))]
    fn new(
        name: &str,
        configs: Option<Vec<CombinedCycleConfig>>,
        transitions: Option<Vec<CombinedCycleTransition>>,
        active_config: Option<String>,
        hours_in_config: f64,
        duct_firing_capable: bool,
    ) -> Self {
        Self {
            name: name.to_string(),
            configs: configs.unwrap_or_default(),
            transitions: transitions.unwrap_or_default(),
            active_config,
            hours_in_config,
            duct_firing_capable,
        }
    }

    fn __repr__(&self) -> String {
        let active = self.active_config.as_deref().unwrap_or("offline");
        format!(
            "<CombinedCyclePlant '{}' configs={} active='{}' duct={}>",
            self.name,
            self.configs.len(),
            active,
            self.duct_firing_capable
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OutageEntry
// ─────────────────────────────────────────────────────────────────────────────

/// An outage or derate event for a piece of equipment.
///
/// Obtain via `net.outage_entries`.
#[pyclass(name = "OutageEntry", skip_from_py_object)]
#[derive(Clone)]
pub struct OutageEntry {
    /// Stable row index in ``network.outage_schedule``.
    #[pyo3(get)]
    pub schedule_index: Option<usize>,
    /// Equipment category string: "Generator", "Branch", "Load", etc.
    #[pyo3(get, set)]
    pub category: String,
    /// Bus number for bus-addressed equipment (generator/load/shunt/breaker).
    #[pyo3(get, set)]
    pub bus: Option<u32>,
    /// Canonical identifier for bus-addressed equipment.
    #[pyo3(get, set)]
    pub id: Option<String>,
    /// From-bus for branch-addressed equipment.
    #[pyo3(get, set)]
    pub from_bus: Option<u32>,
    /// To-bus for branch-addressed equipment.
    #[pyo3(get, set)]
    pub to_bus: Option<u32>,
    /// Circuit identifier for branch-addressed equipment.
    #[pyo3(get, set)]
    pub circuit: Option<String>,
    /// DC grid identifier when applicable.
    #[pyo3(get, set)]
    pub grid_id: Option<u32>,
    /// Named equipment identifier for HVDC links and FACTS devices.
    #[pyo3(get, set)]
    pub name: Option<String>,
    /// LCC terminal side: "Rectifier" or "Inverter".
    #[pyo3(get, set)]
    pub terminal: Option<String>,
    #[pyo3(get, set)]
    pub start_hr: f64,
    #[pyo3(get, set)]
    pub end_hr: f64,
    /// "Planned", "Forced", "Derate", or "Mothballed".
    #[pyo3(get, set)]
    pub outage_type: String,
    #[pyo3(get, set)]
    pub derate_factor: f64,
    #[pyo3(get, set)]
    pub reason: Option<String>,
}

impl OutageEntry {
    pub fn from_core(schedule_index: usize, e: &CoreOutageEntry) -> Self {
        let mut entry = Self {
            schedule_index: Some(schedule_index),
            category: String::new(),
            bus: None,
            id: None,
            from_bus: None,
            to_bus: None,
            circuit: None,
            grid_id: None,
            name: None,
            terminal: None,
            start_hr: e.start_hr,
            end_hr: e.end_hr,
            outage_type: match e.outage_type {
                OutageType::Planned => "Planned",
                OutageType::Forced => "Forced",
                OutageType::Derate => "Derate",
                OutageType::Mothballed => "Mothballed",
            }
            .to_string(),
            derate_factor: e.derate_factor,
            reason: e.reason.clone(),
        };
        match &e.equipment {
            EquipmentRef::Generator(reference) => {
                entry.category = "Generator".to_string();
                entry.bus = Some(reference.bus);
                entry.id = Some(reference.id.clone());
            }
            EquipmentRef::Branch(reference) => {
                entry.category = "Branch".to_string();
                entry.from_bus = Some(reference.from_bus);
                entry.to_bus = Some(reference.to_bus);
                entry.circuit = Some(reference.circuit.clone());
            }
            EquipmentRef::Load(reference) => {
                entry.category = "Load".to_string();
                entry.bus = Some(reference.bus);
                entry.id = Some(reference.id.clone());
            }
            EquipmentRef::Bess(reference) => {
                entry.category = "Bess".to_string();
                entry.bus = Some(reference.bus);
                entry.id = Some(reference.id.clone());
            }
            EquipmentRef::LccHvdcLink(reference) => {
                entry.category = "LccHvdcLink".to_string();
                entry.name = Some(reference.name.clone());
            }
            EquipmentRef::VscHvdcLink(reference) => {
                entry.category = "VscHvdcLink".to_string();
                entry.name = Some(reference.name.clone());
            }
            EquipmentRef::DcGrid(reference) => {
                entry.category = "DcGrid".to_string();
                entry.grid_id = Some(reference.id);
            }
            EquipmentRef::LccConverterTerminal(reference) => {
                entry.category = "LccConverterTerminal".to_string();
                entry.name = Some(reference.link_name.clone());
                entry.terminal = Some(
                    match reference.terminal {
                        surge_network::network::LccTerminalSide::Rectifier => "Rectifier",
                        surge_network::network::LccTerminalSide::Inverter => "Inverter",
                    }
                    .to_string(),
                );
            }
            EquipmentRef::DcBranch(reference) => {
                entry.category = "DcBranch".to_string();
                entry.grid_id = Some(reference.grid_id);
                entry.id = Some(reference.id.clone());
            }
            EquipmentRef::FactsDevice(reference) => {
                entry.category = "FactsDevice".to_string();
                entry.name = Some(reference.name.clone());
            }
            EquipmentRef::SwitchedShunt(reference) => {
                entry.category = "SwitchedShunt".to_string();
                entry.bus = Some(reference.bus);
                entry.id = Some(reference.id.clone());
            }
            EquipmentRef::FixedShunt(reference) => {
                entry.category = "FixedShunt".to_string();
                entry.bus = Some(reference.bus);
                entry.id = Some(reference.id.clone());
            }
            EquipmentRef::InductionMachine(reference) => {
                entry.category = "InductionMachine".to_string();
                entry.bus = Some(reference.bus);
                entry.id = Some(reference.id.clone());
            }
            EquipmentRef::Breaker(reference) => {
                entry.category = "Breaker".to_string();
                entry.bus = Some(reference.bus);
                entry.name = Some(reference.name.clone());
            }
        }
        entry
    }

    pub fn to_core(&self) -> PyResult<CoreOutageEntry> {
        let equipment = match self.category.as_str() {
            "Generator" => EquipmentRef::Generator(GeneratorRef {
                bus: self.bus.ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("Generator outage requires bus")
                })?,
                id: self.id.clone().ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("Generator outage requires id")
                })?,
            }),
            "Branch" => EquipmentRef::Branch(surge_network::network::BranchRef {
                from_bus: self.from_bus.ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("Branch outage requires from_bus")
                })?,
                to_bus: self.to_bus.ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("Branch outage requires to_bus")
                })?,
                circuit: self.circuit.clone().ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("Branch outage requires circuit")
                })?,
            }),
            "Load" => EquipmentRef::Load(surge_network::network::LoadRef {
                bus: self.bus.ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("Load outage requires bus")
                })?,
                id: self.id.clone().ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("Load outage requires id")
                })?,
            }),
            "Bess" => EquipmentRef::Bess(GeneratorRef {
                bus: self.bus.ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("Bess outage requires bus")
                })?,
                id: self.id.clone().ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("Bess outage requires id")
                })?,
            }),
            "LccHvdcLink" => EquipmentRef::LccHvdcLink(surge_network::network::HvdcLinkRef {
                name: self.name.clone().ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("LccHvdcLink outage requires name")
                })?,
            }),
            "VscHvdcLink" => EquipmentRef::VscHvdcLink(surge_network::network::HvdcLinkRef {
                name: self.name.clone().ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("VscHvdcLink outage requires name")
                })?,
            }),
            "DcGrid" => EquipmentRef::DcGrid(surge_network::network::DcGridRef {
                id: self.grid_id.ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("DcGrid outage requires grid_id")
                })?,
            }),
            "LccConverterTerminal" => EquipmentRef::LccConverterTerminal(
                surge_network::network::LccConverterTerminalRef {
                    link_name: self.name.clone().ok_or_else(|| {
                        pyo3::exceptions::PyValueError::new_err(
                            "LccConverterTerminal outage requires name",
                        )
                    })?,
                    terminal: match self.terminal.as_deref() {
                        Some("Rectifier") => surge_network::network::LccTerminalSide::Rectifier,
                        Some("Inverter") => surge_network::network::LccTerminalSide::Inverter,
                        _ => {
                            return Err(pyo3::exceptions::PyValueError::new_err(
                                "LccConverterTerminal outage requires terminal 'Rectifier' or 'Inverter'",
                            ));
                        }
                    },
                },
            ),
            "DcBranch" => EquipmentRef::DcBranch(surge_network::network::DcBranchRef {
                grid_id: self.grid_id.ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("DcBranch outage requires grid_id")
                })?,
                id: self.id.clone().ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("DcBranch outage requires id")
                })?,
            }),
            "FactsDevice" => EquipmentRef::FactsDevice(surge_network::network::FactsDeviceRef {
                name: self.name.clone().ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("FactsDevice outage requires name")
                })?,
            }),
            "SwitchedShunt" => {
                EquipmentRef::SwitchedShunt(surge_network::network::SwitchedShuntRef {
                    bus: self.bus.ok_or_else(|| {
                        pyo3::exceptions::PyValueError::new_err("SwitchedShunt outage requires bus")
                    })?,
                    id: self.id.clone().ok_or_else(|| {
                        pyo3::exceptions::PyValueError::new_err("SwitchedShunt outage requires id")
                    })?,
                })
            }
            "FixedShunt" => EquipmentRef::FixedShunt(surge_network::network::FixedShuntRef {
                bus: self.bus.ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("FixedShunt outage requires bus")
                })?,
                id: self.id.clone().ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("FixedShunt outage requires id")
                })?,
            }),
            "InductionMachine" => {
                EquipmentRef::InductionMachine(surge_network::network::InductionMachineRef {
                    bus: self.bus.ok_or_else(|| {
                        pyo3::exceptions::PyValueError::new_err(
                            "InductionMachine outage requires bus",
                        )
                    })?,
                    id: self.id.clone().ok_or_else(|| {
                        pyo3::exceptions::PyValueError::new_err(
                            "InductionMachine outage requires id",
                        )
                    })?,
                })
            }
            "Breaker" => EquipmentRef::Breaker(surge_network::network::BreakerRef {
                bus: self.bus.ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("Breaker outage requires bus")
                })?,
                name: self.name.clone().ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("Breaker outage requires name")
                })?,
            }),
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unsupported outage category '{other}'"
                )));
            }
        };

        let outage_type = match self.outage_type.as_str() {
            "Planned" => OutageType::Planned,
            "Forced" => OutageType::Forced,
            "Derate" => OutageType::Derate,
            "Mothballed" => OutageType::Mothballed,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "outage_type must be one of Planned, Forced, Derate, Mothballed; got '{other}'"
                )));
            }
        };

        Ok(CoreOutageEntry {
            equipment,
            start_hr: self.start_hr,
            end_hr: self.end_hr,
            outage_type,
            derate_factor: self.derate_factor,
            reason: self.reason.clone(),
        })
    }
}

#[pymethods]
impl OutageEntry {
    #[new]
    #[pyo3(signature = (
        category, start_hr, end_hr,
        outage_type="Planned", derate_factor=0.0, reason=None,
        bus=None, id=None, from_bus=None, to_bus=None, circuit=None, grid_id=None, name=None, terminal=None
    ))]
    fn new(
        category: &str,
        start_hr: f64,
        end_hr: f64,
        outage_type: &str,
        derate_factor: f64,
        reason: Option<String>,
        bus: Option<u32>,
        id: Option<String>,
        from_bus: Option<u32>,
        to_bus: Option<u32>,
        circuit: Option<String>,
        grid_id: Option<u32>,
        name: Option<String>,
        terminal: Option<String>,
    ) -> Self {
        Self {
            schedule_index: None,
            category: category.to_string(),
            bus,
            id,
            from_bus,
            to_bus,
            circuit,
            grid_id,
            name,
            terminal,
            start_hr,
            end_hr,
            outage_type: outage_type.to_string(),
            derate_factor,
            reason,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "<OutageEntry {} {} [{:.1},{:.1}]hr derate={:.2}>",
            self.category, self.outage_type, self.start_hr, self.end_hr, self.derate_factor
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ReserveZone
// ─────────────────────────────────────────────────────────────────────────────

/// A reserve zone with generic zonal reserve requirements.
///
/// Obtain via `net.reserve_zones`.
#[pyclass(name = "ReserveZone", skip_from_py_object)]
#[derive(Clone)]
pub struct ReserveZone {
    #[pyo3(get)]
    pub name: String,
    /// Zonal requirements: list of
    /// (zone_id, product_id, requirement_mw, participant_bus_numbers).
    #[pyo3(get, set)]
    pub zonal_requirements: Vec<(usize, String, f64, Option<Vec<u32>>)>,
}

impl ReserveZone {
    pub fn from_core(z: &CoreReserveZone) -> Self {
        Self {
            name: z.name.clone(),
            zonal_requirements: z
                .zonal_requirements
                .iter()
                .map(|r| {
                    (
                        r.zone_id,
                        r.product_id.clone(),
                        r.requirement_mw,
                        r.participant_bus_numbers.clone(),
                    )
                })
                .collect(),
        }
    }
}

#[pymethods]
impl ReserveZone {
    #[new]
    #[pyo3(signature = (name, zonal_requirements=None))]
    fn new(
        name: &str,
        zonal_requirements: Option<Vec<(usize, String, f64, Option<Vec<u32>>)>>,
    ) -> Self {
        Self {
            name: name.to_string(),
            zonal_requirements: zonal_requirements.unwrap_or_default(),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "<ReserveZone '{}' n_requirements={}>",
            self.name,
            self.zonal_requirements.len()
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Builder functions for new types
// ─────────────────────────────────────────────────────────────────────────────

pub fn pumped_hydro_units_from_network(net: &CoreNetwork) -> Vec<PumpedHydroUnit> {
    net.market_data
        .pumped_hydro_units
        .iter()
        .map(PumpedHydroUnit::from_core)
        .collect()
}

pub fn breaker_ratings_from_network(net: &CoreNetwork) -> Vec<BreakerRating> {
    net.breaker_ratings
        .iter()
        .map(BreakerRating::from_core)
        .collect()
}

pub fn fixed_shunts_from_network(net: &CoreNetwork) -> Vec<FixedShunt> {
    net.fixed_shunts.iter().map(FixedShunt::from_core).collect()
}

pub fn combined_cycle_plants_from_network(net: &CoreNetwork) -> Vec<CombinedCyclePlant> {
    net.market_data
        .combined_cycle_plants
        .iter()
        .map(CombinedCyclePlant::from_core)
        .collect()
}

pub fn outage_entries_from_network(net: &CoreNetwork) -> Vec<OutageEntry> {
    net.market_data
        .outage_schedule
        .iter()
        .enumerate()
        .map(|(schedule_index, entry)| OutageEntry::from_core(schedule_index, entry))
        .collect()
}

pub fn reserve_zones_from_network(net: &CoreNetwork) -> Vec<ReserveZone> {
    net.market_data
        .reserve_zones
        .iter()
        .map(ReserveZone::from_core)
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Module registration
// ─────────────────────────────────────────────────────────────────────────────

/// Register all rich object classes with the Python module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Bus>()?;
    m.add_class::<Branch>()?;
    m.add_class::<StorageParams>()?;
    m.add_class::<Generator>()?;
    m.add_class::<Load>()?;
    m.add_class::<BusSolved>()?;
    m.add_class::<BranchSolved>()?;
    m.add_class::<BusDcSolved>()?;
    m.add_class::<BranchDcSolved>()?;
    m.add_class::<GenSolved>()?;
    m.add_class::<BusOpf>()?;
    m.add_class::<BranchOpf>()?;
    m.add_class::<GenOpf>()?;
    m.add_class::<DcLine>()?;
    m.add_class::<VscDcLine>()?;
    m.add_class::<DcBus>()?;
    m.add_class::<DcBranch>()?;
    m.add_class::<DcConverter>()?;
    m.add_class::<DcGrid>()?;
    m.add_class::<DispatchableLoad>()?;
    m.add_class::<FactsDevice>()?;
    m.add_class::<SwitchedShuntOpf>()?;
    m.add_class::<AreaSchedule>()?;
    m.add_class::<PumpedHydroUnit>()?;
    m.add_class::<BreakerRating>()?;
    m.add_class::<FixedShunt>()?;
    m.add_class::<CombinedCycleConfig>()?;
    m.add_class::<CombinedCycleTransition>()?;
    m.add_class::<CombinedCyclePlant>()?;
    m.add_class::<OutageEntry>()?;
    m.add_class::<ReserveZone>()?;
    Ok(())
}
