# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""
surge — Python bindings for the Surge power flow solver.

Type stubs for IDE autocompletion (PEP 561).
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Callable, Literal, Optional, Union

if TYPE_CHECKING:
    import pandas as pd

import numpy as np
from numpy import ndarray
from numpy.typing import NDArray

# ---------------------------------------------------------------------------
# Exception hierarchy
# ---------------------------------------------------------------------------

class SurgeError(Exception):
    """Base exception for all Surge errors."""
    ...
    def add_note(self) -> Any: ...
    @property
    def args(self) -> Any: ...
    def with_traceback(self) -> Any: ...

class ConvergenceError(SurgeError):
    """Raised when a solver fails to converge."""
    ...
    def add_note(self) -> Any: ...
    @property
    def args(self) -> Any: ...
    def with_traceback(self) -> Any: ...

class InfeasibleError(SurgeError):
    """Raised when an optimization problem is infeasible."""
    ...
    def add_note(self) -> Any: ...
    @property
    def args(self) -> Any: ...
    def with_traceback(self) -> Any: ...

class UnsupportedFeatureError(SurgeError):
    """Raised when a requested workflow or constraint class is not supported."""
    ...
    def add_note(self) -> Any: ...
    @property
    def args(self) -> Any: ...
    def with_traceback(self) -> Any: ...

class NetworkError(SurgeError):
    """Raised when a network element is not found or invalid."""
    ...
    def add_note(self) -> Any: ...
    @property
    def args(self) -> Any: ...
    def with_traceback(self) -> Any: ...

class TopologyError(SurgeError):
    """Base exception for topology and topology-rebuild failures."""
    ...

class MissingTopologyError(TopologyError):
    """Raised when a network has no retained node-breaker topology or no current mapping."""
    ...

class StaleTopologyError(TopologyError):
    """Raised when a topology mapping is stale and must be rebuilt first."""
    ...

class AmbiguousTopologyError(TopologyError):
    """Raised when topology rebuild cannot safely resolve an ambiguous bus split."""
    ...

class TopologyIntegrityError(TopologyError):
    """Raised when retained node-breaker topology data is internally inconsistent."""
    ...

class SurgeIOError(SurgeError):
    """Raised when a file I/O operation fails."""
    ...

# ---------------------------------------------------------------------------
# Topology objects
# ---------------------------------------------------------------------------

TopologyMappingState = Literal["missing", "current", "stale"]

class Substation:
    @property
    def id(self) -> str: ...
    @property
    def name(self) -> str: ...
    @property
    def region(self) -> Optional[str]: ...

class VoltageLevel:
    @property
    def id(self) -> str: ...
    @property
    def name(self) -> str: ...
    @property
    def substation_id(self) -> str: ...
    @property
    def base_kv(self) -> float: ...

class Bay:
    @property
    def id(self) -> str: ...
    @property
    def name(self) -> str: ...
    @property
    def voltage_level_id(self) -> str: ...

class ConnectivityNode:
    @property
    def id(self) -> str: ...
    @property
    def name(self) -> str: ...
    @property
    def voltage_level_id(self) -> str: ...

class BusbarSection:
    @property
    def id(self) -> str: ...
    @property
    def name(self) -> str: ...
    @property
    def connectivity_node_id(self) -> str: ...
    @property
    def ip_max(self) -> Optional[float]: ...

class TerminalConnection:
    @property
    def terminal_id(self) -> str: ...
    @property
    def equipment_id(self) -> str: ...
    @property
    def equipment_class(self) -> str: ...
    @property
    def sequence_number(self) -> int: ...
    @property
    def connectivity_node_id(self) -> str: ...

class TopologySwitch:
    @property
    def id(self) -> str: ...
    @property
    def name(self) -> str: ...
    @property
    def kind(self) -> str: ...
    @property
    def is_open(self) -> bool: ...
    @property
    def normally_open(self) -> bool: ...
    @property
    def retained(self) -> bool: ...
    @property
    def rated_current_amp(self) -> Optional[float]: ...
    @property
    def from_connectivity_node_id(self) -> str: ...
    @property
    def to_connectivity_node_id(self) -> str: ...

class TopologyMapping:
    @property
    def connectivity_node_to_bus(self) -> dict[str, int]: ...
    @property
    def bus_to_connectivity_nodes(self) -> dict[int, list[str]]: ...
    @property
    def consumed_switch_ids(self) -> list[str]: ...
    @property
    def isolated_connectivity_node_ids(self) -> list[str]: ...
    def bus_for_connectivity_node(self, connectivity_node_id: str) -> Optional[int]: ...
    def connectivity_nodes_for_bus(self, bus_number: int) -> Optional[list[str]]: ...

class TopologyBusSplit:
    @property
    def previous_bus_number(self) -> int: ...
    @property
    def current_bus_numbers(self) -> list[int]: ...

class TopologyBusMerge:
    @property
    def current_bus_number(self) -> int: ...
    @property
    def previous_bus_numbers(self) -> list[int]: ...

class CollapsedBranch:
    @property
    def previous_from_bus(self) -> int: ...
    @property
    def previous_to_bus(self) -> int: ...
    @property
    def circuit(self) -> str: ...

class TopologyReport:
    @property
    def previous_bus_count(self) -> int: ...
    @property
    def current_bus_count(self) -> int: ...
    @property
    def bus_splits(self) -> list[TopologyBusSplit]: ...
    @property
    def bus_merges(self) -> list[TopologyBusMerge]: ...
    @property
    def collapsed_branches(self) -> list[CollapsedBranch]: ...
    @property
    def consumed_switch_ids(self) -> list[str]: ...
    @property
    def isolated_connectivity_node_ids(self) -> list[str]: ...

class TopologyRebuildResult:
    @property
    def network(self) -> Network: ...
    @property
    def report(self) -> TopologyReport: ...

class Hvdc:
    @property
    def is_empty(self) -> bool: ...
    @property
    def has_links(self) -> bool: ...
    @property
    def has_explicit_dc_topology(self) -> bool: ...
    @property
    def links(self) -> list[LccHvdcLink | VscHvdcLink]: ...
    @property
    def dc_grids(self) -> list[DcGrid]: ...

class NodeBreakerTopology:
    @property
    def status(self) -> TopologyMappingState: ...
    @property
    def is_current(self) -> bool: ...
    @property
    def substations(self) -> list[Substation]: ...
    @property
    def voltage_levels(self) -> list[VoltageLevel]: ...
    @property
    def bays(self) -> list[Bay]: ...
    @property
    def connectivity_nodes(self) -> list[ConnectivityNode]: ...
    @property
    def busbar_sections(self) -> list[BusbarSection]: ...
    @property
    def switches(self) -> list[TopologySwitch]: ...
    @property
    def terminal_connections(self) -> list[TerminalConnection]: ...
    @property
    def mapping(self) -> Optional[TopologyMapping]: ...
    def current_mapping(self) -> TopologyMapping: ...
    def switch(self, switch_id: str) -> Optional[TopologySwitch]: ...
    def switch_state(self, switch_id: str) -> Optional[bool]: ...
    def set_switch_state(self, switch_id: str, *, is_open: bool) -> bool: ...
    def rebuild(self) -> Network: ...
    def rebuild_with_report(self) -> TopologyRebuildResult: ...

# ---------------------------------------------------------------------------
# Rich element objects (static model + solved results)
# ---------------------------------------------------------------------------

class Bus:
    """A bus (node) in the power system network — all static model fields.

    Obtain via ``net.buses``, ``net.bus(n)``, or ``net.slack_bus``.
    """

    def __init__(
        self,
        number: int,
        bus_type: str = "PQ",
        base_kv: float = 0.0,
        name: str = "",
        pd_mw: float = 0.0,
        qd_mvar: float = 0.0,
        gs_mw: float = 0.0,
        bs_mvar: float = 0.0,
        area: int = 1,
        zone: int = 1,
        vm_pu: float = 1.0,
        va_deg: float = 0.0,
        vmin_pu: float = 0.9,
        vmax_pu: float = 1.1,
        latitude: Optional[float] = None,
        longitude: Optional[float] = None,
    ) -> None: ...

    @property
    def number(self) -> int: ...
    @property
    def name(self) -> str: ...
    @property
    def type_str(self) -> str:
        """Bus type: 'PQ', 'PV', 'Slack', or 'Isolated'."""
        ...
    @property
    def pd_mw(self) -> float: ...
    @property
    def qd_mvar(self) -> float: ...
    @property
    def gs_mw(self) -> float: ...
    @property
    def bs_mvar(self) -> float: ...
    @property
    def area(self) -> int: ...
    @property
    def zone(self) -> int: ...
    @property
    def vm_pu(self) -> float:
        """Initial/flat-start voltage magnitude (p.u.)."""
        ...
    @property
    def va_deg(self) -> float:
        """Initial voltage angle (degrees)."""
        ...
    @property
    def base_kv(self) -> float: ...
    @property
    def vmin_pu(self) -> float: ...
    @property
    def vmax_pu(self) -> float: ...
    @property
    def latitude(self) -> Optional[float]: ...
    @property
    def longitude(self) -> Optional[float]: ...
    @property
    def lam_p(self) -> Optional[float]:
        """OPF LMP energy component ($/MWh). None unless set by OPF."""
        ...
    @property
    def lam_q(self) -> Optional[float]: ...
    @property
    def mu_vmin(self) -> Optional[float]: ...
    @property
    def mu_vmax(self) -> Optional[float]: ...
    # Computed properties
    @property
    def is_slack(self) -> bool: ...
    @property
    def is_pv(self) -> bool: ...
    @property
    def is_pq(self) -> bool: ...
    @property
    def is_isolated(self) -> bool: ...
    @property
    def has_coordinates(self) -> bool: ...
    # Computed engineering properties
    @property
    def vm_kv(self) -> float:
        """Bus voltage in kV = vm_pu * base_kv."""
        ...
    @property
    def s_load_mva(self) -> float:
        """Apparent load |S_load| = sqrt(pd_mw² + qd_mvar²) (MVA)."""
        ...
    @property
    def is_voltage_violated(self) -> bool:
        """True if vm_pu is outside [vmin_pu, vmax_pu]."""
        ...
    @property
    def voltage_deviation_pu(self) -> float:
        """Voltage deviation from nominal: vm_pu - 1.0 (p.u.)."""
        ...
    def __repr__(self) -> str: ...


class Branch:
    """A branch (line or transformer) — all static model fields.

    Obtain via ``net.branches`` or ``net.branch(from_bus, to_bus)``.
    """

    def __init__(
        self,
        from_bus: int,
        to_bus: int,
        circuit: str = "1",
        r_pu: float = 0.0,
        x_pu: float = 0.0,
        b_pu: float = 0.0,
        rate_a_mva: float = 0.0,
        rate_b_mva: float = 0.0,
        rate_c_mva: float = 0.0,
        tap: float = 1.0,
        shift_deg: float = 0.0,
        in_service: bool = True,
        angmin_deg: Optional[float] = None,
        angmax_deg: Optional[float] = None,
        g_pi: float = 0.0,
        g_mag: float = 0.0,
        b_mag: float = 0.0,
        transformer_connection: str = "WyeG-WyeG",
        delta_connected: bool = False,
        skin_effect_alpha: float = 0.0,
        tap_mode: str = "Fixed",
        tap_min: float = 0.9,
        tap_max: float = 1.1,
        phase_mode: str = "Fixed",
        phase_min_deg: float = 0.0,
        phase_max_deg: float = 0.0,
        oil_temp_limit_c: Optional[float] = None,
        winding_temp_limit_c: Optional[float] = None,
        impedance_limit_ohm: Optional[float] = None,
        has_saturation: bool = False,
        core_type: Optional[str] = None,
    ) -> None: ...

    @property
    def from_bus(self) -> int: ...
    @property
    def to_bus(self) -> int: ...
    @property
    def circuit(self) -> str: ...
    @property
    def r_pu(self) -> float: ...
    @property
    def x_pu(self) -> float: ...
    @property
    def b_pu(self) -> float:
        """Total line charging susceptance (p.u.)."""
        ...
    @property
    def rate_a_mva(self) -> float: ...
    @property
    def rate_b_mva(self) -> float: ...
    @property
    def rate_c_mva(self) -> float: ...
    @property
    def tap(self) -> float:
        """Off-nominal tap ratio. 1.0 for lines."""
        ...
    @property
    def shift_deg(self) -> float:
        """Phase shift angle (degrees). 0.0 for lines."""
        ...
    @property
    def in_service(self) -> bool: ...
    @property
    def angmin_deg(self) -> Optional[float]: ...
    @property
    def angmax_deg(self) -> Optional[float]: ...
    @property
    def g_pi(self) -> float:
        """Line charging conductance (p.u.)."""
        ...
    @property
    def g_mag(self) -> float:
        """Transformer magnetizing conductance (p.u.)."""
        ...
    @property
    def b_mag(self) -> float:
        """Transformer magnetizing susceptance (p.u.)."""
        ...
    @property
    def transformer_connection(self) -> str:
        """Winding connection string, e.g. 'WyeG-WyeG', 'WyeG-Delta'."""
        ...
    @property
    def tap_mode(self) -> str:
        """Tap control mode: 'Fixed' or 'Continuous'."""
        ...
    @property
    def tap_min(self) -> float: ...
    @property
    def tap_max(self) -> float: ...
    @property
    def phase_mode(self) -> str: ...
    @property
    def phase_min_deg(self) -> float: ...
    @property
    def phase_max_deg(self) -> float: ...
    @property
    def skin_effect_alpha(self) -> float: ...
    @property
    def oil_temp_limit_c(self) -> Optional[float]: ...
    @property
    def winding_temp_limit_c(self) -> Optional[float]: ...
    # Computed properties
    @property
    def is_transformer(self) -> bool:
        """True if tap != 1.0 or shift_deg != 0.0."""
        ...
    @property
    def z_pu(self) -> tuple[float, float]:
        """Series impedance (r_pu, x_pu) as a tuple."""
        ...
    @property
    def b_dc_pu(self) -> float:
        """DC power flow susceptance: 1 / (x_pu * tap)."""
        ...
    # Computed engineering properties
    @property
    def x_r_ratio(self) -> float:
        """X/R ratio of series impedance. Large for high-voltage lines."""
        ...
    @property
    def z_mag_pu(self) -> float:
        """Series impedance magnitude |Z| = sqrt(r² + x²) (p.u.)."""
        ...
    @property
    def y_series_pu(self) -> tuple[float, float]:
        """Series admittance y = 1/z as (g_series, b_series) p.u."""
        ...
    def __repr__(self) -> str: ...
    @property
    def delta_connected(self) -> Any: ...
    @property
    def impedance_limit_ohm(self) -> float: ...
    @property
    def has_saturation(self) -> bool: ...
    @property
    def core_type(self) -> str | None: ...
    @property
    def mu_angmax(self) -> Any: ...
    @property
    def mu_angmin(self) -> Any: ...
    @property
    def mu_sf(self) -> Any: ...
    @property
    def mu_st(self) -> Any: ...
    @property
    def pf_mw(self) -> float: ...
    @property
    def pt_mw(self) -> float: ...
    @property
    def qf_mvar(self) -> float: ...
    @property
    def qt_mvar(self) -> float: ...


class StorageParams:
    """Storage parameters for a generator-backed storage resource."""

    def __init__(
        self,
        energy_capacity_mwh: float,
        charge_efficiency: Optional[float] = None,
        discharge_efficiency: Optional[float] = None,
        efficiency: Optional[float] = None,
        soc_initial_mwh: Optional[float] = None,
        soc_min_mwh: float = 0.0,
        soc_max_mwh: Optional[float] = None,
        variable_cost_per_mwh: float = 0.0,
        degradation_cost_per_mwh: float = 0.0,
        dispatch_mode: str = "cost_minimization",
        self_schedule_mw: float = 0.0,
        discharge_offer: Optional[list[tuple[float, float]]] = None,
        charge_bid: Optional[list[tuple[float, float]]] = None,
        max_c_rate_charge: Optional[float] = None,
        max_c_rate_discharge: Optional[float] = None,
        chemistry: Optional[str] = None,
        discharge_foldback_soc_mwh: Optional[float] = None,
        charge_foldback_soc_mwh: Optional[float] = None,
    ) -> None: ...

    @property
    def charge_efficiency(self) -> float: ...
    @charge_efficiency.setter
    def charge_efficiency(self, value: float) -> None: ...
    @property
    def discharge_efficiency(self) -> float: ...
    @discharge_efficiency.setter
    def discharge_efficiency(self, value: float) -> None: ...
    @property
    def round_trip_efficiency(self) -> float: ...
    @property
    def energy_capacity_mwh(self) -> float: ...
    @energy_capacity_mwh.setter
    def energy_capacity_mwh(self, value: float) -> None: ...
    @property
    def soc_initial_mwh(self) -> float: ...
    @soc_initial_mwh.setter
    def soc_initial_mwh(self, value: float) -> None: ...
    @property
    def soc_min_mwh(self) -> float: ...
    @soc_min_mwh.setter
    def soc_min_mwh(self, value: float) -> None: ...
    @property
    def soc_max_mwh(self) -> float: ...
    @soc_max_mwh.setter
    def soc_max_mwh(self, value: float) -> None: ...
    @property
    def variable_cost_per_mwh(self) -> float: ...
    @variable_cost_per_mwh.setter
    def variable_cost_per_mwh(self, value: float) -> None: ...
    @property
    def degradation_cost_per_mwh(self) -> float: ...
    @degradation_cost_per_mwh.setter
    def degradation_cost_per_mwh(self, value: float) -> None: ...
    @property
    def dispatch_mode(self) -> str: ...
    @dispatch_mode.setter
    def dispatch_mode(self, value: str) -> None: ...
    @property
    def self_schedule_mw(self) -> float: ...
    @self_schedule_mw.setter
    def self_schedule_mw(self, value: float) -> None: ...
    @property
    def discharge_offer(self) -> Optional[list[tuple[float, float]]]: ...
    @discharge_offer.setter
    def discharge_offer(self, value: Optional[list[tuple[float, float]]]) -> None: ...
    @property
    def charge_bid(self) -> Optional[list[tuple[float, float]]]: ...
    @charge_bid.setter
    def charge_bid(self, value: Optional[list[tuple[float, float]]]) -> None: ...
    @property
    def max_c_rate_charge(self) -> Optional[float]: ...
    @max_c_rate_charge.setter
    def max_c_rate_charge(self, value: Optional[float]) -> None: ...
    @property
    def max_c_rate_discharge(self) -> Optional[float]: ...
    @max_c_rate_discharge.setter
    def max_c_rate_discharge(self, value: Optional[float]) -> None: ...
    @property
    def chemistry(self) -> Optional[str]: ...
    @chemistry.setter
    def chemistry(self, value: Optional[str]) -> None: ...
    @property
    def discharge_foldback_soc_mwh(self) -> Optional[float]: ...
    @discharge_foldback_soc_mwh.setter
    def discharge_foldback_soc_mwh(self, value: Optional[float]) -> None: ...
    @property
    def charge_foldback_soc_mwh(self) -> Optional[float]: ...
    @charge_foldback_soc_mwh.setter
    def charge_foldback_soc_mwh(self, value: Optional[float]) -> None: ...

    def __repr__(self) -> str: ...


class Generator:
    """A generator connected to a bus — all static model fields.

    Obtain via ``net.generators`` or ``net.generator(bus)``.
    """

    def __init__(self, bus: int, machine_id: str = "1") -> None: ...

    @property
    def bus(self) -> int: ...
    @property
    def machine_id(self) -> str: ...
    @property
    def p_mw(self) -> float:
        """Model dispatch (MW). Not the solved or OPF dispatch."""
        ...
    @property
    def q_mvar(self) -> float: ...
    @property
    def pmax_mw(self) -> float: ...
    @property
    def pmin_mw(self) -> float: ...
    @property
    def qmax_mvar(self) -> float: ...
    @property
    def qmin_mvar(self) -> float: ...
    @property
    def vs_pu(self) -> float:
        """Voltage setpoint (p.u.)."""
        ...
    @property
    def mbase_mva(self) -> float: ...
    @property
    def in_service(self) -> bool: ...
    @property
    def fuel_type(self) -> Optional[str]: ...
    @property
    def heat_rate_btu_mwh(self) -> Optional[float]: ...
    @property
    def co2_rate_t_per_mwh(self) -> float: ...
    @property
    def nox_rate_t_per_mwh(self) -> float: ...
    @property
    def so2_rate_t_per_mwh(self) -> float: ...
    @property
    def pm25_rate_t_per_mwh(self) -> float: ...
    @property
    def forced_outage_rate(self) -> Optional[float]: ...
    # Ramp curves — (MW operating-point, MW/min) segments
    @property
    def ramp_up_curve(self) -> list[tuple[float, float]]: ...
    @property
    def ramp_down_curve(self) -> list[tuple[float, float]]: ...
    @property
    def reg_ramp_up_curve(self) -> list[tuple[float, float]]: ...
    # Scalar ramp helpers (from first segment of curves)
    @property
    def ramp_up_mw_per_min(self) -> Optional[float]: ...
    @property
    def ramp_dn_mw_per_min(self) -> Optional[float]: ...
    @property
    def ramp_agc_mw_per_min(self) -> Optional[float]: ...
    # Commitment
    @property
    def commitment_status(self) -> str:
        """Commitment status: "Market", "SelfCommitted", "MustRun", "Unavailable", "EmergencyOnly"."""
        ...
    @property
    def must_run(self) -> bool:
        """True if commitment_status is "MustRun"."""
        ...
    @property
    def min_up_time_hr(self) -> Optional[float]: ...
    @property
    def min_down_time_hr(self) -> Optional[float]: ...
    @property
    def max_up_time_hr(self) -> Optional[float]: ...
    @property
    def min_run_at_pmin_hr(self) -> Optional[float]: ...
    @property
    def max_starts_per_day(self) -> Optional[int]: ...
    @property
    def max_starts_per_week(self) -> Optional[int]: ...
    @property
    def max_energy_mwh_per_day(self) -> Optional[float]: ...
    @property
    def startup_cost_tiers(self) -> list[tuple[float, float, float]]:
        """Startup cost tiers: [(max_offline_hr, cost_$, sync_time_min), ...]."""
        ...
    @startup_cost_tiers.setter
    def startup_cost_tiers(self, value: list[tuple[float, float, float]]) -> None: ...
    @property
    def quick_start(self) -> bool: ...
    # Reserve offers: list of (product_id, capacity_mw, cost_per_mwh)
    @property
    def reserve_offers(self) -> list[tuple[str, float, float]]: ...
    # Reserve qualification flags: {product_id: qualified}
    @property
    def qualifications(self) -> dict[str, bool]: ...
    # Reactive capability
    @property
    def pc1_mw(self) -> Optional[float]: ...
    @property
    def pc2_mw(self) -> Optional[float]: ...
    @property
    def qc1min_mvar(self) -> Optional[float]: ...
    @property
    def qc1max_mvar(self) -> Optional[float]: ...
    @property
    def qc2min_mvar(self) -> Optional[float]: ...
    @property
    def qc2max_mvar(self) -> Optional[float]: ...
    @property
    def pq_curve(self) -> list[tuple[float, float, float]]:
        """P-Q capability curve points: [(p_pu, qmax_pu, qmin_pu), ...]."""
        ...
    # Dynamics
    @property
    def h_inertia_s(self) -> Optional[float]: ...
    @property
    def xs_pu(self) -> Optional[float]: ...
    @property
    def apf(self) -> Optional[float]: ...
    # Fault analysis
    @property
    def x2_pu(self) -> Optional[float]:
        """Negative-sequence subtransient reactance X2 (p.u., machine base). None if not set."""
        ...
    @property
    def zn_pu(self) -> Optional[tuple[float, float]]:
        """Neutral grounding impedance Zn as (re, im) p.u. on system base. None if solidly grounded."""
        ...
    # Cost
    @property
    def cost_model(self) -> Optional[str]:
        """'polynomial', 'piecewise_linear', or None."""
        ...
    @property
    def cost_startup(self) -> float: ...
    @property
    def cost_shutdown(self) -> float: ...
    @property
    def cost_coefficients(self) -> list[float]: ...
    @property
    def cost_breakpoints_mw(self) -> list[float]: ...
    @property
    def cost_breakpoints_usd(self) -> list[float]: ...
    # OPF duals
    @property
    def mu_pmin(self) -> Optional[float]: ...
    @property
    def mu_pmax(self) -> Optional[float]: ...
    @property
    def mu_qmin(self) -> Optional[float]: ...
    @property
    def mu_qmax(self) -> Optional[float]: ...
    # Computed methods
    def cost_at(self, p_mw: float) -> float:
        """Total generation cost ($/hr) at dispatch p_mw."""
        ...
    def marginal_cost_at(self, p_mw: float) -> float:
        """Marginal cost ($/MWh) — derivative of cost curve at p_mw."""
        ...
    @property
    def has_cost(self) -> bool:
        """True if a cost model is available."""
        ...
    @property
    def cost_c0(self) -> Optional[float]:
        """Constant term c₀ of polynomial cost f(P)=c2·P²+c1·P+c0 ($/hr). None if not polynomial."""
        ...
    @property
    def cost_c1(self) -> Optional[float]:
        """Linear coefficient c₁ ($/MWh). None if not polynomial or fewer than 2 coefficients."""
        ...
    @property
    def cost_c2(self) -> Optional[float]:
        """Quadratic coefficient c₂ ($/MW²·hr). None if not polynomial or fewer than 3 coefficients."""
        ...
    @property
    def has_reactive_capability_curve(self) -> bool:
        """True if pq_curve is non-empty."""
        ...
    @property
    def has_ancillary_services(self) -> bool:
        """True if any ancillary service (reg_up/dn/nspin/ecrs/rrs) > 0."""
        ...
    # Computed engineering properties
    @property
    def capacity_mw(self) -> float:
        """Installed capacity = pmax_mw - pmin_mw (MW)."""
        ...
    @property
    def headroom_mw(self) -> float:
        """Available headroom = pmax_mw - p_mw (MW)."""
        ...
    @property
    def reactive_range_mvar(self) -> float:
        """Total reactive range = qmax_mvar - qmin_mvar (MVAr)."""
        ...
    @property
    def power_factor(self) -> float:
        """Scheduled power factor = p_mw / sqrt(p_mw² + q_mvar²). 1.0 if both zero."""
        ...
    def __repr__(self) -> str: ...


class Load:
    """A load connected to a bus.

    Obtain via ``net.loads``.
    """

    def __init__(
        self,
        bus: int,
        id: str = "1",
        pd_mw: float = 0.0,
        qd_mvar: float = 0.0,
        in_service: bool = True,
        conforming: bool = True,
    ) -> None: ...

    @property
    def bus(self) -> int: ...
    @property
    def id(self) -> str: ...
    @property
    def pd_mw(self) -> float: ...
    @property
    def qd_mvar(self) -> float: ...
    @property
    def in_service(self) -> bool: ...
    @property
    def conforming(self) -> bool:
        """True if this load follows system-wide forecast scaling."""
        ...
    def __repr__(self) -> str: ...


class BusSolved:
    """Bus with power flow solution results.

    All static ``Bus`` fields plus solved voltage and injection results.
    Obtain via ``sol.get_buses(net)``, ``sol.bus(net, n)``.
    """

    # Static fields (same as Bus)
    @property
    def number(self) -> int: ...
    @property
    def name(self) -> str: ...
    @property
    def type_str(self) -> str: ...
    @property
    def pd_mw(self) -> float: ...
    @property
    def qd_mvar(self) -> float: ...
    @property
    def area(self) -> int: ...
    @property
    def base_kv(self) -> float: ...
    @property
    def vmin_pu(self) -> float: ...
    @property
    def vmax_pu(self) -> float: ...
    @property
    def is_slack(self) -> bool: ...
    @property
    def is_pv(self) -> bool: ...
    @property
    def is_pq(self) -> bool: ...
    # Solved fields
    @property
    def vm_pu(self) -> float:
        """Solved voltage magnitude (p.u.)."""
        ...
    @property
    def va_deg(self) -> float:
        """Solved voltage angle (degrees)."""
        ...
    @property
    def p_inject_mw(self) -> float:
        """Net active power injection = Pg - Pd (MW)."""
        ...
    @property
    def q_inject_mvar(self) -> float:
        """Net reactive power injection = Qg - Qd (MVAr)."""
        ...
    @property
    def p_load_mw(self) -> float:
        """Bus active power load demand (MW). Alias for pd_mw."""
        ...
    @property
    def q_load_mvar(self) -> float:
        """Bus reactive power load demand (MVAr). Alias for qd_mvar."""
        ...
    @property
    def island_id(self) -> int:
        """Island (connected component) index. 0 when single island."""
        ...
    @property
    def q_limited(self) -> bool:
        """True if this bus hit a Q limit (PV→PQ switch) during the solve."""
        ...
    # Computed engineering properties
    @property
    def vm_kv(self) -> float:
        """Solved voltage in kV = vm_pu * base_kv."""
        ...
    @property
    def v_rect(self) -> tuple[float, float]:
        """Solved voltage phasor in rectangular form (vr, vi) p.u."""
        ...
    @property
    def s_inject_mva(self) -> float:
        """Apparent power injection |S| = sqrt(p_inject_mw² + q_inject_mvar²) (MVA)."""
        ...
    @property
    def s_load_mva(self) -> float:
        """Apparent load |S_load| = sqrt(pd_mw² + qd_mvar²) (MVA)."""
        ...
    @property
    def is_voltage_violated(self) -> bool:
        """True if solved vm_pu is outside [vmin_pu, vmax_pu]."""
        ...
    @property
    def voltage_deviation_pu(self) -> float:
        """Voltage deviation from nominal: vm_pu - 1.0 (p.u.)."""
        ...
    def __repr__(self) -> str: ...
    @property
    def bs_mvar(self) -> float: ...
    @property
    def gs_mw(self) -> float: ...
    @property
    def has_coordinates(self) -> bool: ...
    @property
    def is_isolated(self) -> bool: ...
    @property
    def latitude(self) -> Any: ...
    @property
    def longitude(self) -> Any: ...
    @property
    def zone(self) -> int: ...


class BranchSolved:
    """Branch with power flow result flows.

    All static ``Branch`` fields plus from/to-end power flows.
    Obtain via ``sol.get_branches(net)``.
    """

    # Static fields (same as Branch)
    @property
    def from_bus(self) -> int: ...
    @property
    def to_bus(self) -> int: ...
    @property
    def circuit(self) -> str: ...
    @property
    def r_pu(self) -> float: ...
    @property
    def x_pu(self) -> float: ...
    @property
    def rate_a_mva(self) -> float: ...
    @property
    def tap(self) -> float: ...
    @property
    def shift_deg(self) -> float: ...
    @property
    def in_service(self) -> bool: ...
    @property
    def g_mag(self) -> float:
        """Transformer magnetizing conductance (p.u.)."""
        ...
    @property
    def b_mag(self) -> float:
        """Transformer magnetizing susceptance (p.u.)."""
        ...
    @property
    def transformer_connection(self) -> str:
        """Winding connection string, e.g. 'WyeG-WyeG', 'WyeG-Delta'."""
        ...
    @property
    def angmin_deg(self) -> Optional[float]:
        """Minimum angle difference constraint (degrees). None if not set."""
        ...
    @property
    def angmax_deg(self) -> Optional[float]:
        """Maximum angle difference constraint (degrees). None if not set."""
        ...
    @property
    def is_transformer(self) -> bool: ...
    # Solved fields
    @property
    def pf_mw(self) -> float:
        """From-end active power flow (MW). Positive = flowing from→to."""
        ...
    @property
    def qf_mvar(self) -> float:
        """From-end reactive power flow (MVAr)."""
        ...
    @property
    def pt_mw(self) -> float:
        """To-end active power flow (MW). Positive = flowing to→from."""
        ...
    @property
    def qt_mvar(self) -> float:
        """To-end reactive power flow (MVAr)."""
        ...
    @property
    def loading_pct(self) -> float:
        """Thermal loading as % of Rate A. 0.0 if rate_a == 0."""
        ...
    @property
    def losses_mw(self) -> float:
        """Active power line losses = pf_mw + pt_mw (MW)."""
        ...
    # Computed engineering properties
    @property
    def sf_mva(self) -> float:
        """From-end apparent power |Sf| = sqrt(pf_mw² + qf_mvar²) (MVA)."""
        ...
    @property
    def st_mva(self) -> float:
        """To-end apparent power |St| = sqrt(pt_mw² + qt_mvar²) (MVA)."""
        ...
    @property
    def losses_mvar(self) -> float:
        """Reactive power line losses = qf_mvar + qt_mvar (MVAr)."""
        ...
    @property
    def headroom_mva(self) -> float:
        """Remaining thermal headroom = rate_a_mva - max(sf_mva, st_mva) (MVA). NaN if unrated."""
        ...
    @property
    def headroom_pct(self) -> float:
        """Thermal headroom as a percentage of Rate A. 100.0 if unrated."""
        ...
    @property
    def is_overloaded(self) -> bool:
        """True if loading_pct > 100.0."""
        ...
    @property
    def x_r_ratio(self) -> float:
        """X/R ratio of series impedance."""
        ...
    def __repr__(self) -> str: ...
    @property
    def b_dc_pu(self) -> float: ...
    @property
    def b_pu(self) -> float: ...
    @property
    def rate_b_mva(self) -> float: ...
    @property
    def rate_c_mva(self) -> float: ...


class GenSolved:
    """Generator with power flow solved reactive power output.

    All static ``Generator`` fields plus solved Qg.
    Obtain via ``sol.get_generators(net)``.
    """

    # Static fields (all Generator fields)
    @property
    def bus(self) -> int: ...
    @property
    def machine_id(self) -> str: ...
    @property
    def p_mw(self) -> float: ...
    @property
    def q_mvar(self) -> float: ...
    @property
    def pmax_mw(self) -> float: ...
    @property
    def pmin_mw(self) -> float: ...
    @property
    def qmax_mvar(self) -> float: ...
    @property
    def qmin_mvar(self) -> float: ...
    @property
    def vs_pu(self) -> float: ...
    @property
    def mbase_mva(self) -> float: ...
    @property
    def in_service(self) -> bool: ...
    @property
    def fuel_type(self) -> Optional[str]: ...
    @property
    def heat_rate_btu_mwh(self) -> Optional[float]: ...
    @property
    def co2_rate_t_per_mwh(self) -> float: ...
    @property
    def nox_rate_t_per_mwh(self) -> float: ...
    @property
    def so2_rate_t_per_mwh(self) -> float: ...
    @property
    def pm25_rate_t_per_mwh(self) -> float: ...
    @property
    def forced_outage_rate(self) -> Optional[float]: ...
    @property
    def ramp_up_curve(self) -> list[tuple[float, float]]: ...
    @property
    def ramp_down_curve(self) -> list[tuple[float, float]]: ...
    @property
    def reg_ramp_up_curve(self) -> list[tuple[float, float]]: ...
    @property
    def ramp_up_mw_per_min(self) -> Optional[float]: ...
    @property
    def ramp_dn_mw_per_min(self) -> Optional[float]: ...
    @property
    def ramp_agc_mw_per_min(self) -> Optional[float]: ...
    @property
    def commitment_status(self) -> str: ...
    @property
    def must_run(self) -> bool: ...
    @property
    def min_up_time_hr(self) -> Optional[float]: ...
    @property
    def min_down_time_hr(self) -> Optional[float]: ...
    @property
    def max_up_time_hr(self) -> Optional[float]: ...
    @property
    def min_run_at_pmin_hr(self) -> Optional[float]: ...
    @property
    def max_starts_per_day(self) -> Optional[int]: ...
    @property
    def max_starts_per_week(self) -> Optional[int]: ...
    @property
    def max_energy_mwh_per_day(self) -> Optional[float]: ...
    @property
    def startup_cost_tiers(self) -> list[tuple[float, float, float]]: ...
    @property
    def quick_start(self) -> bool: ...
    @property
    def reserve_offers(self) -> list[tuple[str, float, float]]: ...
    @property
    def qualifications(self) -> dict[str, bool]: ...
    @property
    def pc1_mw(self) -> Optional[float]: ...
    @property
    def pc2_mw(self) -> Optional[float]: ...
    @property
    def qc1min_mvar(self) -> Optional[float]: ...
    @property
    def qc1max_mvar(self) -> Optional[float]: ...
    @property
    def qc2min_mvar(self) -> Optional[float]: ...
    @property
    def qc2max_mvar(self) -> Optional[float]: ...
    @property
    def pq_curve(self) -> list[tuple[float, float, float]]: ...
    @property
    def h_inertia_s(self) -> Optional[float]: ...
    @property
    def xs_pu(self) -> Optional[float]: ...
    @property
    def apf(self) -> Optional[float]: ...
    @property
    def x2_pu(self) -> Optional[float]: ...
    @property
    def zn_pu(self) -> Optional[tuple[float, float]]: ...
    # Cost model fields
    @property
    def cost_model(self) -> Optional[str]: ...
    @property
    def cost_startup(self) -> float: ...
    @property
    def cost_shutdown(self) -> float: ...
    @property
    def cost_coefficients(self) -> list[float]: ...
    @property
    def cost_breakpoints_mw(self) -> list[float]: ...
    @property
    def cost_breakpoints_usd(self) -> list[float]: ...
    # OPF duals
    @property
    def mu_pmin(self) -> Optional[float]: ...
    @property
    def mu_pmax(self) -> Optional[float]: ...
    @property
    def mu_qmin(self) -> Optional[float]: ...
    @property
    def mu_qmax(self) -> Optional[float]: ...
    # Solved field
    @property
    def q_mvar_solved(self) -> float:
        """Post-solve reactive power output (MVAr), computed from bus Q injection."""
        ...
    # Computed methods
    @property
    def has_cost(self) -> bool: ...
    @property
    def cost_c0(self) -> Optional[float]:
        """Constant term c₀ of polynomial cost f(P)=c2·P²+c1·P+c0 ($/hr). None if not polynomial."""
        ...
    @property
    def cost_c1(self) -> Optional[float]:
        """Linear coefficient c₁ ($/MWh). None if not polynomial."""
        ...
    @property
    def cost_c2(self) -> Optional[float]:
        """Quadratic coefficient c₂ ($/MW²·hr). None if not polynomial."""
        ...
    @property
    def has_reactive_capability_curve(self) -> bool: ...
    @property
    def has_ancillary_services(self) -> bool: ...
    def cost_at(self, p_mw: float) -> float: ...
    def marginal_cost_at(self, p_mw: float) -> float: ...
    def __repr__(self) -> str: ...


class BusOpf:
    """Bus with OPF locational marginal prices and voltage solution.

    All static ``Bus`` fields plus LMPs and shadow prices.
    Obtain via ``opf_result.get_buses(net)``.
    """

    # Static fields
    @property
    def number(self) -> int: ...
    @property
    def name(self) -> str: ...
    @property
    def type_str(self) -> str: ...
    @property
    def pd_mw(self) -> float: ...
    @property
    def base_kv(self) -> float: ...
    @property
    def area(self) -> int: ...
    @property
    def is_slack(self) -> bool: ...
    # OPF results
    @property
    def vm_pu(self) -> float:
        """Solved voltage magnitude (p.u.). 1.0 for DC-OPF."""
        ...
    @property
    def va_deg(self) -> float:
        """Solved voltage angle (degrees)."""
        ...
    @property
    def lmp(self) -> float:
        """Locational marginal price ($/MWh)."""
        ...
    @property
    def lmp_energy(self) -> float:
        """Energy component of LMP ($/MWh). Uniform for DC-OPF."""
        ...
    @property
    def lmp_congestion(self) -> float:
        """Congestion component of LMP ($/MWh). 0.0 for uncongested buses."""
        ...
    @property
    def lmp_loss(self) -> float:
        """Loss component of LMP ($/MWh). 0.0 for DC-OPF."""
        ...
    @property
    def lmp_reactive(self) -> float:
        """Reactive LMP ($/MVAr-h). 0.0 for DC-OPF."""
        ...
    @property
    def mu_vmin(self) -> float:
        """Shadow price on voltage minimum constraint. 0.0 if not binding."""
        ...
    @property
    def mu_vmax(self) -> float:
        """Shadow price on voltage maximum constraint. 0.0 if not binding."""
        ...
    # Computed engineering properties
    @property
    def load_payment_per_hr(self) -> float:
        """Load payment rate = pd_mw * lmp ($/hr)."""
        ...
    @property
    def is_congested(self) -> bool:
        """True if |lmp_congestion| > 1e-3 (bus has a non-trivial congestion component)."""
        ...
    @property
    def is_voltage_constrained(self) -> bool:
        """True if mu_vmin > 1e-6 or mu_vmax > 1e-6 (voltage bound is binding)."""
        ...
    def __repr__(self) -> str: ...
    @property
    def is_pq(self) -> bool: ...
    @property
    def is_pv(self) -> bool: ...
    @property
    def qd_mvar(self) -> float: ...
    @property
    def vmax_pu(self) -> float: ...
    @property
    def vmin_pu(self) -> float: ...
    @property
    def zone(self) -> int: ...


class BranchOpf:
    """Branch with OPF power flows and shadow prices.

    All static ``Branch`` fields plus OPF flow results.
    Obtain via ``opf_result.get_branches(net)``.
    """

    # Static fields
    @property
    def from_bus(self) -> int: ...
    @property
    def to_bus(self) -> int: ...
    @property
    def circuit(self) -> str: ...
    @property
    def r_pu(self) -> float: ...
    @property
    def x_pu(self) -> float: ...
    @property
    def rate_a_mva(self) -> float: ...
    @property
    def rate_b_mva(self) -> float:
        """Short-term (Rate B) thermal rating (MVA)."""
        ...
    @property
    def rate_c_mva(self) -> float:
        """Emergency (Rate C) thermal rating (MVA)."""
        ...
    @property
    def in_service(self) -> bool: ...
    @property
    def g_mag(self) -> float:
        """Transformer magnetizing conductance (p.u.)."""
        ...
    @property
    def b_mag(self) -> float:
        """Transformer magnetizing susceptance (p.u.)."""
        ...
    @property
    def transformer_connection(self) -> str:
        """Winding connection string, e.g. 'WyeG-WyeG', 'WyeG-Delta'."""
        ...
    @property
    def angmin_deg(self) -> Optional[float]:
        """Minimum angle difference constraint (degrees). None if not set."""
        ...
    @property
    def angmax_deg(self) -> Optional[float]:
        """Maximum angle difference constraint (degrees). None if not set."""
        ...
    @property
    def is_transformer(self) -> bool: ...
    # OPF results
    @property
    def pf_mw(self) -> float:
        """From-end active power flow (MW)."""
        ...
    @property
    def qf_mvar(self) -> float:
        """From-end reactive power flow (MVAr). 0.0 for DC-OPF."""
        ...
    @property
    def pt_mw(self) -> float:
        """To-end active power flow (MW)."""
        ...
    @property
    def qt_mvar(self) -> float:
        """To-end reactive power flow (MVAr). 0.0 for DC-OPF."""
        ...
    @property
    def loading_pct(self) -> float:
        """Thermal loading as % of Rate A."""
        ...
    @property
    def losses_mw(self) -> float:
        """Active power losses pf_mw + pt_mw (MW). Zero for DC-OPF (lossless)."""
        ...
    @property
    def shadow_price(self) -> float:
        """Thermal constraint shadow price ($/MWh per MW)."""
        ...
    @property
    def mu_angmin(self) -> float:
        """Shadow price on minimum angle constraint. 0.0 if not binding."""
        ...
    @property
    def mu_angmax(self) -> float:
        """Shadow price on maximum angle constraint. 0.0 if not binding."""
        ...
    @property
    def is_binding(self) -> bool:
        """True if |shadow_price| > 1e-6 (thermal limit is binding)."""
        ...
    # Computed engineering properties
    @property
    def sf_mva(self) -> float:
        """From-end apparent power |Sf| = sqrt(pf_mw² + qf_mvar²) (MVA)."""
        ...
    @property
    def st_mva(self) -> float:
        """To-end apparent power |St| = sqrt(pt_mw² + qt_mvar²) (MVA)."""
        ...
    @property
    def headroom_mva(self) -> float:
        """Remaining thermal headroom = rate_a_mva - max(sf_mva, st_mva) (MVA). NaN if unrated."""
        ...
    def __repr__(self) -> str: ...
    @property
    def b_pu(self) -> float: ...
    @property
    def shift_deg(self) -> float: ...
    @property
    def tap(self) -> Any: ...


class GenOpf:
    """Generator with OPF dispatch and KKT multipliers.

    All static ``Generator`` fields plus OPF dispatch results.
    Obtain via ``opf_result.get_generators(net)``.
    """

    # Static fields
    @property
    def bus(self) -> int: ...
    @property
    def machine_id(self) -> str: ...
    @property
    def pmax_mw(self) -> float: ...
    @property
    def pmin_mw(self) -> float: ...
    @property
    def qmax_mvar(self) -> float: ...
    @property
    def qmin_mvar(self) -> float: ...
    @property
    def vs_pu(self) -> float:
        """Voltage setpoint (p.u.)."""
        ...
    @property
    def mbase_mva(self) -> float: ...
    @property
    def in_service(self) -> bool: ...
    @property
    def fuel_type(self) -> Optional[str]: ...
    @property
    def co2_rate_t_per_mwh(self) -> float: ...
    @property
    def has_cost(self) -> bool: ...
    @property
    def cost_model(self) -> Optional[str]: ...
    @property
    def cost_startup(self) -> float: ...
    @property
    def cost_shutdown(self) -> float: ...
    @property
    def cost_coefficients(self) -> list[float]: ...
    @property
    def cost_breakpoints_mw(self) -> list[float]: ...
    @property
    def cost_breakpoints_usd(self) -> list[float]: ...
    @property
    def cost_c0(self) -> Optional[float]:
        """Constant term c₀ of polynomial cost f(P)=c2·P²+c1·P+c0 ($/hr). None if not polynomial."""
        ...
    @property
    def cost_c1(self) -> Optional[float]:
        """Linear coefficient c₁ ($/MWh). None if not polynomial."""
        ...
    @property
    def cost_c2(self) -> Optional[float]:
        """Quadratic coefficient c₂ ($/MW²·hr). None if not polynomial."""
        ...
    # OPF results
    @property
    def p_mw(self) -> float:
        """OPF optimal active power dispatch (MW)."""
        ...
    @property
    def q_mvar(self) -> float:
        """OPF optimal reactive power dispatch (MVAr). 0.0 for DC-OPF."""
        ...
    @property
    def mu_pmin(self) -> float:
        """Shadow price on lower active power bound ($/MWh). 0.0 if not binding."""
        ...
    @property
    def mu_pmax(self) -> float:
        """Shadow price on upper active power bound ($/MWh). 0.0 if not binding."""
        ...
    @property
    def mu_qmin(self) -> float:
        """Shadow price on lower reactive bound ($/MWh). 0.0 for DC-OPF."""
        ...
    @property
    def mu_qmax(self) -> float:
        """Shadow price on upper reactive bound ($/MWh). 0.0 for DC-OPF."""
        ...
    @property
    def cost_actual(self) -> float:
        """Actual dispatch cost at optimal p_mw ($/hr)."""
        ...
    # Computed engineering properties
    def cost_at(self, p_mw: float) -> float:
        """Total generation cost ($/hr) at dispatch p_mw."""
        ...
    def marginal_cost_at(self, p_mw: float) -> float:
        """Marginal cost ($/MWh) — derivative of cost curve at p_mw."""
        ...
    @property
    def dispatch_pct(self) -> float:
        """Dispatch as % of capacity = (p_mw - pmin_mw) / (pmax_mw - pmin_mw) * 100."""
        ...
    @property
    def headroom_mw(self) -> float:
        """Available upward headroom = pmax_mw - p_mw (MW)."""
        ...
    @property
    def is_at_pmax(self) -> bool:
        """True if p_mw >= pmax_mw - 0.01 (at upper limit)."""
        ...
    @property
    def is_at_pmin(self) -> bool:
        """True if p_mw <= pmin_mw + 0.01 (at lower limit)."""
        ...
    def __repr__(self) -> str: ...


# ---------------------------------------------------------------------------
# Equipment classes (HVDC, FACTS, DR, area interchange)
# ---------------------------------------------------------------------------
class LccHvdcLink:
    """An LCC-HVDC (classical) DC transmission line.

    Obtain via ``net.hvdc.links``.
    """

    def __init__(
        self,
        name: str,
        rectifier_bus: int,
        inverter_bus: int,
        scheduled_setpoint: float = 0.0,
        scheduled_voltage_kv: float = 500.0,
        resistance_ohm: float = 0.0,
        in_service: bool = True,
    ) -> None: ...

    @property
    def name(self) -> str:
        """DC line name (from PSS/E VSCHED field or blank)."""
        ...
    @property
    def scheduled_setpoint(self) -> float:
        """Scheduled flow setpoint (MW or kA depending on control mode)."""
        ...
    @property
    def scheduled_voltage_kv(self) -> float:
        """Scheduled DC voltage (kV)."""
        ...
    @property
    def resistance_ohm(self) -> float:
        """DC line resistance (ohms)."""
        ...
    @property
    def rectifier_bus(self) -> int:
        """External bus number of the rectifier (sending) terminal."""
        ...
    @property
    def inverter_bus(self) -> int:
        """External bus number of the inverter (receiving) terminal."""
        ...
    @property
    def in_service(self) -> bool: ...
    # Computed
    @property
    def p_mw(self) -> float:
        """Scheduled DC power transfer (MW). Alias for setvl when in MW control mode."""
        ...
    def __repr__(self) -> str: ...


class VscHvdcLink:
    """A VSC-HVDC DC transmission line (two voltage-source converters).

    Obtain via ``net.hvdc.links``.
    """

    def __init__(
        self,
        name: str,
        converter1_bus: int,
        converter2_bus: int,
        p_mw: float = 0.0,
        loss_a_mw: float = 0.0,
        loss_linear: float = 0.0,
        resistance_ohm: float = 0.0,
        in_service: bool = True,
        q1_min_mvar: float = -9999.0,
        q1_max_mvar: float = 9999.0,
        q2_min_mvar: float = -9999.0,
        q2_max_mvar: float = 9999.0,
    ) -> None: ...

    @property
    def name(self) -> str: ...
    @property
    def p_mw(self) -> float:
        """Scheduled active power transfer (MW)."""
        ...
    @property
    def loss_a_mw(self) -> float:
        """Constant loss component (MW) of converter 1."""
        ...
    @property
    def loss_linear(self) -> float:
        """Variable loss coefficient (MW/MW)."""
        ...
    @property
    def resistance_ohm(self) -> float:
        """DC line resistance (ohms)."""
        ...
    @property
    def converter1_bus(self) -> int:
        """External bus number of converter 1."""
        ...
    @property
    def converter2_bus(self) -> int:
        """External bus number of converter 2."""
        ...
    @property
    def in_service(self) -> bool: ...
    @property
    def q1_min_mvar(self) -> float:
        """Minimum reactive power at converter 1 (MVAr)."""
        ...
    @property
    def q1_max_mvar(self) -> float:
        """Maximum reactive power at converter 1 (MVAr)."""
        ...
    @property
    def q2_min_mvar(self) -> float:
        """Minimum reactive power at converter 2 (MVAr)."""
        ...
    @property
    def q2_max_mvar(self) -> float:
        """Maximum reactive power at converter 2 (MVAr)."""
        ...
    def __repr__(self) -> str: ...


class DcBus:
    """An explicit DC bus in a canonical DC grid."""

    dc_bus: int
    p_dc_mw: float
    v_dc_pu: float
    base_kv_dc: float
    v_dc_min_pu: float
    v_dc_max_pu: float
    g_shunt_siemens: float
    r_ground_ohm: float

    def __repr__(self) -> str: ...


class DcBranch:
    """An explicit DC branch in a canonical DC grid."""

    from_bus: int
    to_bus: int
    resistance_ohm: float
    rating_a_mw: float
    rating_b_mw: float
    rating_c_mw: float
    in_service: bool

    def __repr__(self) -> str: ...


class DcConverter:
    """An explicit AC/DC converter station in a canonical DC grid."""

    dc_bus: int
    ac_bus: int
    technology: str
    dc_control_mode: str
    ac_control_mode: str
    power_dc_setpoint_mw: float
    reactive_power_mvar: float
    voltage_dc_setpoint_pu: float
    voltage_setpoint_pu: float
    droop_mw_per_pu: float
    loss_constant_mw: float
    loss_linear: float
    in_service: bool

    def __repr__(self) -> str: ...


class DcGrid:
    """A canonical explicit DC grid."""

    grid_id: int
    name: str | None
    buses: list[DcBus]
    branches: list[DcBranch]
    converters: list[DcConverter]

    def __repr__(self) -> str: ...


class DispatchableLoad:
    """A dispatchable load (demand response) resource.

    Obtain via ``net.dispatchable_loads``.

    Python creation/update currently supports ``"Curtailable"`` and
    ``"Interruptible"`` archetypes.
    """

    def __init__(
        self,
        bus: int,
        archetype: str = "Curtailable",
        p_sched_mw: float = 0.0,
        q_sched_mvar: float = 0.0,
        pmin_mw: float = 0.0,
        pmax_mw: float = 0.0,
        qmin_mvar: float = 0.0,
        qmax_mvar: float = 0.0,
        fixed_power_factor: bool = True,
        in_service: bool = True,
        product_type: Optional[str] = None,
        baseline_mw: Optional[float] = None,
        cost_per_mwh: Optional[float] = None,
        reserve_offers: Optional[list[tuple[str, float, float]]] = None,
        qualifications: Optional[dict[str, bool]] = None,
    ) -> None: ...

    @property
    def index(self) -> Optional[int]: ...

    @property
    def bus(self) -> int:
        """External bus number where the resource is connected."""
        ...
    @property
    def p_sched_mw(self) -> float:
        """Scheduled active power consumption (MW)."""
        ...
    @property
    def q_sched_mvar(self) -> float:
        """Scheduled reactive power consumption (MVAr)."""
        ...
    @property
    def pmin_mw(self) -> float:
        """Minimum dispatchable active power (MW)."""
        ...
    @property
    def pmax_mw(self) -> float:
        """Maximum dispatchable active power (MW)."""
        ...
    @property
    def qmin_mvar(self) -> float:
        """Minimum reactive power (MVAr)."""
        ...
    @property
    def qmax_mvar(self) -> float:
        """Maximum reactive power (MVAr)."""
        ...
    @property
    def archetype(self) -> str:
        """Load archetype: 'residential', 'commercial', 'industrial', or 'generic'."""
        ...
    @property
    def fixed_power_factor(self) -> bool:
        """True if reactive power is fixed proportional to active power."""
        ...
    @property
    def in_service(self) -> bool: ...
    @property
    def product_type(self) -> Optional[str]:
        """Market product type (e.g. 'energy', 'demand_response'). None if not set."""
        ...
    @property
    def baseline_mw(self) -> Optional[float]: ...
    @property
    def cost_per_mwh(self) -> Optional[float]: ...
    @property
    def reserve_offers(self) -> list[tuple[str, float, float]]:
        """Reserve offers as ``(product_id, capacity_mw, cost_per_mwh)`` tuples."""
        ...
    @property
    def qualifications(self) -> dict[str, bool]:
        """Reserve qualification flags keyed by product id."""
        ...
    # Computed
    @property
    def is_generator(self) -> bool:
        """True if archetype is 'generator' (producing rather than consuming)."""
        ...
    def __repr__(self) -> str: ...


class FactsDevice:
    """A FACTS device (SVC, STATCOM, TCSC, UPFC, or similar).

    Obtain via ``net.facts_devices``.
    """

    def __init__(
        self,
        name: str,
        bus_from: int,
        bus_to: int = 0,
        mode: str = "ShuntOnly",
        p_des_mw: float = 0.0,
        q_des_mvar: float = 0.0,
        v_set_pu: float = 1.0,
        q_max_mvar: float = 9999.0,
        linx_pu: float = 0.0,
        in_service: bool = True,
    ) -> None: ...

    @property
    def name(self) -> str:
        """Device name string."""
        ...
    @property
    def bus_from(self) -> int:
        """External bus number of the shunt end (sending bus)."""
        ...
    @property
    def bus_to(self) -> int:
        """External bus number of the series end (receiving bus). 0 for shunt-only devices."""
        ...
    @property
    def mode(self) -> str:
        """Control mode string, e.g. 'VoltageControl', 'ReactivePowerControl'."""
        ...
    @property
    def p_des_mw(self) -> float:
        """Desired active power setpoint (MW)."""
        ...
    @property
    def q_des_mvar(self) -> float:
        """Desired reactive power setpoint (MVAr)."""
        ...
    @property
    def v_set_pu(self) -> float:
        """Voltage setpoint (p.u.)."""
        ...
    @property
    def q_max_mvar(self) -> float:
        """Maximum reactive power capability (MVAr)."""
        ...
    @property
    def linx_pu(self) -> float:
        """Series reactance (p.u.) for series-connected FACTS."""
        ...
    @property
    def in_service(self) -> bool: ...
    # Computed
    @property
    def has_shunt(self) -> bool:
        """True if this device has a shunt component (bus_from connected)."""
        ...
    @property
    def has_series(self) -> bool:
        """True if this device has a series component (bus_to > 0)."""
        ...
    def __repr__(self) -> str: ...


class SwitchedShuntOpf:
    """OPF-dispatched switched shunt (capacitor/reactor) result.

    Obtain via ``opf_result.switched_shunts(net)``.
    """

    @property
    def bus(self) -> int:
        """External bus number of the switched shunt."""
        ...
    @property
    def b_min_pu(self) -> float:
        """Minimum susceptance (p.u.). Negative = inductive."""
        ...
    @property
    def b_max_pu(self) -> float:
        """Maximum susceptance (p.u.). Positive = capacitive."""
        ...
    @property
    def b_dispatch_pu(self) -> float:
        """OPF-optimal continuous susceptance (p.u.)."""
        ...
    @property
    def b_rounded_pu(self) -> float:
        """Rounded discrete susceptance (p.u.) nearest to b_dispatch_pu."""
        ...
    @property
    def q_mvar(self) -> float:
        """Reactive power injected = b_dispatch_pu * base_mva (MVAr). Positive = capacitive."""
        ...
    def __repr__(self) -> str: ...


class AreaSchedule:
    """An area interchange control record.

    Obtain via ``net.area_schedules``.
    """

    def __init__(
        self,
        area: int,
        slack_bus: int,
        p_desired_mw: float = 0.0,
        p_tolerance_mw: float = 10.0,
        name: str = "",
    ) -> None: ...

    @property
    def area(self) -> int:
        """Area number."""
        ...
    @property
    def slack_bus(self) -> int:
        """Area slack bus (external bus number)."""
        ...
    @property
    def p_desired_mw(self) -> float:
        """Desired net export from this area (MW). Positive = exporting."""
        ...
    @property
    def p_tolerance_mw(self) -> float:
        """Interchange tolerance dead-band (MW)."""
        ...
    @property
    def name(self) -> str:
        """Area name string."""
        ...
    def __repr__(self) -> str: ...


# ---------------------------------------------------------------------------
# Pure-Python classes
# ---------------------------------------------------------------------------


# Core classes
# ---------------------------------------------------------------------------

class Network:
    """A power system network (buses, branches, generators, loads).

    Can be constructed empty or loaded from a file::

        net = surge.Network()                    # empty network
        net = surge.Network("my grid", 100.0)    # with name and base MVA
        net = surge.load("case9.m")              # from file
    """

    def __init__(
        self,
        name: str = "",
        base_mva: float = 100.0,
        freq_hz: float = 60.0,
    ) -> None:
        """Create an empty network.

        Parameters
        ----------
        name : str, optional
            Network name (default ``""``).
        base_mva : float, optional
            System base MVA (default ``100.0``).
        freq_hz : float, optional
            Nominal frequency in Hz (default ``60.0``).
        """
        ...

    @property
    def name(self) -> str: ...
    @name.setter
    def name(self, value: str) -> None: ...
    @property
    def base_mva(self) -> float: ...
    @base_mva.setter
    def base_mva(self, value: float) -> None: ...
    @property
    def freq_hz(self) -> float: ...
    @freq_hz.setter
    def freq_hz(self, value: float) -> None: ...
    @property
    def n_buses(self) -> int: ...
    @property
    def n_branches(self) -> int: ...
    @property
    def n_generators(self) -> int: ...
    @property
    def total_generation_mw(self) -> float: ...
    @property
    def total_load_mw(self) -> float: ...
    @property
    def bus_numbers(self) -> list[int]: ...
    @property
    def bus_pd(self) -> list[float]: ...
    @property
    def bus_vm(self) -> list[float]: ...
    @property
    def bus_coordinates(self) -> list[tuple[float, float] | None]:
        """Per-bus (latitude, longitude) in decimal degrees (WGS84), or None where absent.

        Populated by the PSS/E, CGMES, and XIIDM parsers from coordinates embedded in
        the case file. Returns None for buses where coordinates are unavailable.
        """
        ...
    @property
    def gen_buses(self) -> list[int]: ...
    @property
    def gen_p(self) -> list[float]: ...
    @property
    def gen_pmax(self) -> list[float]: ...
    @property
    def gen_pmin(self) -> list[float]: ...
    @property
    def gen_in_service(self) -> list[bool]: ...
    @property
    def branch_from(self) -> list[int]: ...
    @property
    def branch_to(self) -> list[int]: ...
    @property
    def branch_rate_a(self) -> list[float]: ...
    @property
    def branch_r(self) -> list[float]: ...
    @property
    def branch_x(self) -> list[float]: ...

    # --- Phase 1: Extended property arrays ---
    @property
    def bus_area(self) -> list[int]:
        """Area number per bus (same order as bus_numbers)."""
        ...
    @property
    def bus_zone(self) -> list[int]:
        """Zone number per bus."""
        ...
    @property
    def bus_base_kv(self) -> list[float]:
        """Nominal base kV per bus."""
        ...
    @property
    def bus_name(self) -> list[str]:
        """Name string per bus."""
        ...
    @property
    def bus_type_str(self) -> list[str]:
        """Bus type per bus: 'PQ' | 'PV' | 'Slack' | 'Isolated'."""
        ...
    @property
    def bus_qd(self) -> list[float]:
        """Reactive load (MVAr) per bus."""
        ...
    @property
    def bus_vmin(self) -> list[float]:
        """Minimum voltage limit (p.u.) per bus."""
        ...
    @property
    def bus_vmax(self) -> list[float]:
        """Maximum voltage limit (p.u.) per bus."""
        ...
    @property
    def bus_gs(self) -> list[float]:
        """Shunt conductance (MW at 1 p.u.) per bus."""
        ...
    @property
    def bus_bs(self) -> list[float]:
        """Shunt susceptance (MVAr at 1 p.u.) per bus."""
        ...
    @property
    def branch_b(self) -> list[float]:
        """Line charging susceptance (p.u.) per branch."""
        ...
    @property
    def branch_rate_b(self) -> list[float]:
        """Short-term (Rate B) thermal rating (MVA) per branch."""
        ...
    @property
    def branch_rate_c(self) -> list[float]:
        """Emergency (Rate C) thermal rating (MVA) per branch."""
        ...
    @property
    def branch_in_service(self) -> list[bool]:
        """In-service flag per branch."""
        ...
    @property
    def branch_tap(self) -> list[float]:
        """Transformer tap ratio (1.0 for lines) per branch."""
        ...
    @property
    def branch_shift_deg(self) -> list[float]:
        """Phase shift angle (degrees) per branch."""
        ...
    @property
    def branch_circuit(self) -> list[str]:
        """Parallel circuit identifier per branch."""
        ...
    @property
    def branch_rated_mvar_series(self) -> list[float | None]:
        """Rated MVAr of series compensation element per branch (None if N/A)."""
        ...
    @property
    def branch_bypassed(self) -> list[bool]:
        """Whether each branch's series compensator is currently bypassed."""
        ...
    @property
    def branch_bypass_current_ka(self) -> list[float | None]:
        """Bypass current threshold (kA) for series capacitor protection per branch."""
        ...
    @property
    def gen_qmax(self) -> list[float]:
        """Reactive power upper limit (MVAr) per generator."""
        ...
    @property
    def gen_qmin(self) -> list[float]:
        """Reactive power lower limit (MVAr) per generator."""
        ...
    @property
    def gen_vs_pu(self) -> list[float]:
        """Voltage setpoint (p.u.) per generator."""
        ...
    @property
    def gen_machine_id(self) -> list[str]:
        """Machine identifier string per generator."""
        ...
    @property
    def gen_q(self) -> list[float]:
        """Model reactive power output (MVAr) per generator (pre-solve model value)."""
        ...

    # --- Scaling methods ---
    def scale_loads(self, factor: float, area: Optional[int] = None) -> None:
        """Scale all bus loads (and Load objects) by ``factor``.

        Args:
            factor: Multiplicative scale factor (e.g. 0.5 to halve all load).
            area: If given, only scale buses in this area number.
        """
        ...
    def scale_generators(self, factor: float, area: Optional[int] = None) -> None:
        """Scale all in-service generator dispatch by ``factor``.

        Args:
            factor: Multiplicative scale factor.
            area: If given, only scale generators whose bus is in this area number.
        """
        ...

    # --- Phase 1: Tabular methods ---
    def bus_dataframe(self) -> pd.DataFrame:
        """Return a pandas DataFrame of bus data (or dict if pandas is not installed).

        Columns: bus_id, name, type, base_kv, area, zone, pd_mw, qd_mvar,
        gs_mw, bs_mvar, vmin_pu, vmax_pu, vm_pu, va_deg, latitude, longitude.
        """
        ...
    def branch_dataframe(self) -> pd.DataFrame:
        """Return a pandas DataFrame of branch data (or dict if pandas is not installed).

        Columns: from_bus, to_bus, circuit, r, x, b, rate_a_mva, rate_b_mva,
        rate_c_mva, tap, shift_deg, in_service.
        """
        ...
    def gen_dataframe(self) -> pd.DataFrame:
        """Return a pandas DataFrame of generator data (or dict if pandas is not installed).

        Index: MultiIndex ``(bus_id, machine_id)``.
        Columns: gen_idx, p_mw, q_mvar, pmax_mw, pmin_mw,
        qmax_mvar, qmin_mvar, vs_pu, in_service, fuel_type.
        """
        ...
    def buses(
        self,
        area: Optional[list[int]] = None,
        zone: Optional[list[int]] = None,
        kv_min: Optional[float] = None,
        kv_max: Optional[float] = None,
        bus_type: Optional[str] = None,
    ) -> list[int]:
        """Return external bus numbers matching all specified filters.

        All filters are optional; omitting a filter means "no constraint".
        bus_type: 'PQ' | 'PV' | 'Slack' | 'Isolated'.
        """
        ...

    # --- Phase 5: OLTC and switched shunt controls ---
    @property
    def n_oltc_controls(self) -> int:
        """Number of registered OLTC tap controls."""
        ...
    @property
    def n_switched_shunts(self) -> int:
        """Number of registered switched shunt controls."""
        ...
    def add_oltc_control(
        self,
        from_bus: int,
        to_bus: int,
        circuit: int | str = "1",
        v_target: float = 1.0,
        v_band: float = 0.01,
        tap_min: float = 0.9,
        tap_max: float = 1.1,
        tap_step: float = 0.00625,
        regulated_bus: Optional[int] = None,
    ) -> None:
        """Register an On-Load Tap-Changer (OLTC) voltage control.

        Args:
            from_bus: External from-bus number of the controlled transformer.
            to_bus: External to-bus number.
            circuit: Circuit identifier (default 1).
            v_target: Voltage target in p.u. (default 1.0).
            v_band: Dead-band half-width in p.u. (default 0.01).
            tap_min: Minimum tap ratio (default 0.9).
            tap_max: Maximum tap ratio (default 1.1).
            tap_step: Discrete tap step size (default 0.00625 = 16 steps/side).
            regulated_bus: External bus to regulate; defaults to to_bus.
        """
        ...
    def add_switched_shunt(
        self,
        bus: int,
        b_step_mvar: float,
        n_steps_cap: int = 0,
        n_steps_react: int = 0,
        v_target: float = 1.0,
        v_band: float = 0.02,
    ) -> None:
        """Register a switched shunt (capacitor/reactor bank) voltage control.

        Args:
            bus: External bus number.
            b_step_mvar: Susceptance per step in MVAr at 1.0 p.u. (positive = capacitive).
            n_steps_cap: Maximum capacitor steps (default 0).
            n_steps_react: Maximum reactor steps (default 0).
            v_target: Voltage target in p.u. (default 1.0).
            v_band: Dead-band half-width in p.u. (default 0.02).
        """
        ...
    def clear_discrete_controls(self) -> None:
        """Remove all registered OLTC and switched shunt controls."""
        ...

    # --- Phase 6: Island detection ---
    def islands(self) -> list[list[int]]:
        """Return connected component groups as lists of external bus numbers.

        Example: [[1, 2, 3], [4, 5]] means buses 1-3 and 4-5 are in separate islands.
        """
        ...

    # --- Phase 9: TIER 3 methods ---
    def area_schedule_mw(self, solution: "AcPfResult") -> dict[int, float]:
        """Compute net MW export per area from post-PF branch flows.

        Returns a dict mapping area number → net export in MW (positive = exporting).
        """
        ...
    def compare_with(self, other: "Network") -> pd.DataFrame:
        """Compare this network to another and return a summary of differences.

        Returns a dict with keys:
          'buses'    — list of {bus_id, field, old, new} change dicts
          'branches' — list of {from_bus, to_bus, circuit, field, old, new} change dicts
        """
        ...
    def generator_capability_curve(self, id: str) -> list[tuple[float, float, float]]:
        """Return the P-Q capability curve for a generator.

        Returns list of (p_pu, qmax_pu, qmin_pu) tuples.
        Empty list if no curve data is available.
        """
        ...

    def validate(self) -> None:
        """Validate network data integrity.

        Raises ``NetworkError`` if any data inconsistencies are found.
        """
        ...

    def ybus(self) -> YBusResult:
        """Build the bus admittance matrix (Y-bus) as a sparse CSC complex matrix.

        Returns a YBusResult with CSC arrays and ``to_scipy()`` for
        ``scipy.sparse.csc_matrix`` (complex128) conversion.
        """
        ...

    def jacobian(
        self, vm: NDArray[np.float64], va_rad: NDArray[np.float64]
    ) -> JacobianResult:
        """Build the power flow Jacobian at the given voltage state.

        Args:
            vm: Bus voltage magnitudes (p.u.), length n_buses.
            va_rad: Bus voltage angles (radians), length n_buses.

        Returns:
            JacobianResult with sparse CSC arrays and bus classification metadata.
        """
        ...

    # --- Substation topology ---
    @property
    def topology(self) -> Optional[NodeBreakerTopology]:
        """Retained node-breaker topology, or None for bus-branch-only networks."""
        ...

    def __repr__(self) -> str: ...

    # --- Network editing ---
    def add_bus(
        self,
        number: int,
        bus_type: str,
        base_kv: float,
        name: str = "",
        pd_mw: float = 0.0,
        qd_mvar: float = 0.0,
        vm_pu: float = 1.0,
        va_deg: float = 0.0,
    ) -> None:
        """Add a bus. bus_type: 'PQ' | 'PV' | 'Slack' | 'Isolated'."""
        ...
    def remove_bus(self, number: int) -> None:
        """Remove bus and all connected branches, generators, loads."""
        ...
    def set_bus_type(self, bus: int, bus_type: str) -> None:
        """Set bus type. bus_type: 'PQ' | 'PV' | 'Slack' | 'Isolated'."""
        ...
    def canonicalize_runtime_identities(self) -> None:
        """Canonicalize runtime identities after topology/service edits."""
        ...
    def set_bus_load(self, bus: int, pd_mw: float, qd_mvar: float = 0.0) -> None:
        """Set active and reactive load at a bus (MW, MVAr)."""
        ...
    def set_bus_voltage(self, bus: int, vm_pu: float, va_deg: float = 0.0) -> None:
        """Set voltage setpoint at a bus (p.u., degrees)."""
        ...
    def set_bus_shunt(self, bus: int, gs_mw: float, bs_mvar: float) -> None:
        """Set shunt admittance at a bus (MW, MVAr at 1.0 p.u.)."""
        ...
    def add_bus_object(self, bus: Bus) -> None:
        """Add a bus from an editable ``Bus`` object."""
        ...
    def update_bus_object(self, bus: Bus) -> None:
        """Apply an editable ``Bus`` object back onto the network."""
        ...
    def add_branch(
        self,
        from_bus: int,
        to_bus: int,
        r: float,
        x: float,
        b: float = 0.0,
        rate_a_mva: float = 0.0,
        tap: float = 1.0,
        shift_deg: float = 0.0,
        circuit: int | str = "1",
        skin_effect_alpha: float = 0.0,
        delta_connected: bool = False,
    ) -> None:
        """Add a branch (line or transformer) in p.u.

        Args:
            skin_effect_alpha: IEC 60287 skin-effect coefficient. If > 0,
                R_h = R * (1 + alpha * (h-1)). If 0 (default), R_h = R * sqrt(h).
            delta_connected: If True, block triplen harmonics (3rd, 6th, ...).
        """
        ...
    def add_line(
        self,
        from_bus: int,
        to_bus: int,
        r_ohm_per_km: float,
        x_ohm_per_km: float,
        b_us_per_km: float,
        length_km: float,
        base_kv: float,
        rate_a_mva: float = 0.0,
        circuit: int | str = "1",
    ) -> None:
        """Add a transmission line from physical (engineering) parameters.

        Converts conductor parameters in Ohm/km and uS/km to per-unit using
        ``z_base = base_kv^2 / base_mva``.

        Parameters
        ----------
        from_bus : int
            From-bus number.
        to_bus : int
            To-bus number.
        r_ohm_per_km : float
            Series resistance (Ohm/km).
        x_ohm_per_km : float
            Series reactance (Ohm/km).
        b_us_per_km : float
            Shunt susceptance (micro-Siemens per km).
        length_km : float
            Line length in km.
        base_kv : float
            Nominal voltage (kV) for per-unit conversion.
        rate_a_mva : float, optional
            Thermal limit (MVA). 0 = unconstrained (default).
        circuit : int or str, optional
            Circuit identifier for parallel lines (default 1).
        """
        ...
    def add_transformer(
        self,
        from_bus: int,
        to_bus: int,
        mva_rating: float,
        v1_kv: float,
        v2_kv: float,
        z_percent: float,
        r_percent: float = 0.5,
        tap_pu: float = 1.0,
        shift_deg: float = 0.0,
        rate_a_mva: float = 0.0,
        circuit: int | str = "1",
    ) -> None:
        """Add a transformer from nameplate (engineering) parameters.

        Converts percent impedance on the transformer's own MVA base to per-unit
        on the system base (``base_mva``, typically 100 MVA).

        Parameters
        ----------
        from_bus : int
            HV (primary) bus number.
        to_bus : int
            LV (secondary) bus number.
        mva_rating : float
            Transformer MVA rating.
        v1_kv : float
            Primary (from_bus) rated voltage (kV).
        v2_kv : float
            Secondary (to_bus) rated voltage (kV).
        z_percent : float
            Impedance in percent on transformer MVA base (e.g. 8.0 for 8%).
        r_percent : float, optional
            Resistance in percent on transformer MVA base (default 0.5).
        tap_pu : float, optional
            Off-nominal tap ratio in p.u. (default 1.0).
        shift_deg : float, optional
            Phase shift angle in degrees (default 0.0).
        rate_a_mva : float, optional
            Thermal rating in MVA. 0 = use mva_rating (default).
        circuit : int or str, optional
            Circuit identifier (default 1).
        """
        ...
    def remove_branch(self, from_bus: int, to_bus: int, circuit: int | str = "1") -> None:
        """Remove a branch."""
        ...
    def set_branch_in_service(
        self, from_bus: int, to_bus: int, in_service: bool, circuit: int | str = "1"
    ) -> None:
        """Set a branch in- or out-of-service."""
        ...
    def set_branch_tap(
        self, from_bus: int, to_bus: int, tap: float, circuit: int | str = "1"
    ) -> None:
        """Set transformer tap ratio."""
        ...
    def set_branch_harmonic_params(
        self,
        from_bus: int,
        to_bus: int,
        skin_effect_alpha: float | None = None,
        delta_connected: bool | None = None,
        circuit: int | str = "1",
    ) -> None:
        """Set harmonic analysis parameters on an existing branch.

        Args:
            skin_effect_alpha: IEC 60287 skin-effect coefficient.
                If > 0: R_h = R * (1 + alpha * (h-1)). If 0: R_h = R * sqrt(h).
            delta_connected: If True, block triplen harmonics (3rd, 6th, ...).
        """
        ...
    def set_branch_rating(
        self, from_bus: int, to_bus: int, rate_a_mva: float, circuit: int | str = "1"
    ) -> None:
        """Set branch long-term thermal rating (MVA)."""
        ...
    def set_branch_ratings(
        self,
        from_bus: int,
        to_bus: int,
        rate_a_mva: float,
        rate_b_mva: float,
        rate_c_mva: float,
        circuit: int | str = "1",
    ) -> None:
        """Set all three thermal ratings (A/B/C) for a branch simultaneously.

        - Rate A: Long-term continuous rating (normal operations).
        - Rate B: Short-term emergency rating (NERC post-contingency).
        - Rate C: Operator-defined emergency rating.

        Raises:
            NetworkError: if the branch is not found.
        """
        ...
    def set_branch_additional_shunt(
        self,
        from_bus: int,
        to_bus: int,
        g_from_pu: float,
        b_from_pu: float,
        g_to_pu: float,
        b_to_pu: float,
        circuit: int | str = "1",
    ) -> None:
        """Set the per-side (asymmetric) shunt admittance additions for a branch.

        GO Competition Challenge 3 §4.8 eqs (148)-(151) allow AC lines and
        transformers to carry distinct shunt-to-ground components at each
        terminal. The four values are additions on top of the symmetric
        ``b/2`` / ``g_pi/2`` split stored in ``Branch::b`` / ``Branch::g_pi``.
        Default 0.0 preserves the symmetric pi-model.

        Raises:
            NetworkError: if the branch is not found.
        """
        ...
    def set_branch_transition_costs(
        self,
        from_bus: int,
        to_bus: int,
        startup: float,
        shutdown: float,
        circuit: int | str = "1",
    ) -> None:
        """Set branch switching transition costs (``c^su_j``, ``c^sd_j``).

        GO Competition Challenge 3 §4.4.6 eqs (62)-(63) price branch
        startup/shutdown indicators at a fixed cost per transition. The
        GO C3 data format surfaces these as ``connection_cost`` and
        ``disconnection_cost`` on AC line and transformer records.

        Only consulted by SCUC when ``allow_branch_switching = true``.

        Raises:
            NetworkError: if the branch is not found.
        """
        ...
    def set_branch_impedance(
        self,
        from_bus: int,
        to_bus: int,
        circuit: int | str = "1",
        r_pu: float | None = None,
        x_pu: float | None = None,
        b_pu: float | None = None,
    ) -> bool:
        """Set branch impedance parameters (p.u.).

        Only non-None values are updated. Returns True if the branch was found.
        """
        ...
    def set_branch_sequence(
        self,
        from_bus: int,
        to_bus: int,
        circuit: int | str,
        r0: float,
        x0: float,
        b0: float = 0.0,
    ) -> bool:
        """Set zero-sequence impedance for a branch."""
        ...
    def get_branch_sequence(
        self, from_bus: int, to_bus: int, circuit: int | str
    ) -> Optional[tuple[float, float, float]]:
        """Get zero-sequence impedance for a branch."""
        ...
    def add_branch_object(self, branch: Branch) -> None:
        """Add a branch from an editable ``Branch`` object."""
        ...
    def update_branch_object(self, branch: Branch) -> None:
        """Apply an editable ``Branch`` object back onto the network."""
        ...
    def add_generator(
        self,
        bus: int,
        p_mw: float,
        pmax_mw: float,
        pmin_mw: float = 0.0,
        vs_pu: float = 1.0,
        qmax_mvar: float = 9999.0,
        qmin_mvar: float = -9999.0,
        machine_id: str = "1",
        id: str | None = None,
    ) -> str:
        """Add a generator at a bus. Returns the generator's assigned ID."""
        ...
    def remove_generator(self, id: str) -> None:
        """Remove a generator by its ID (e.g. ``"gen_1_1"``)."""
        ...
    def set_generator_p(self, id: str, p_mw: float) -> None:
        """Set generator active power output (MW)."""
        ...
    def set_generator_in_service(self, id: str, in_service: bool) -> None:
        """Set generator in-service status."""
        ...
    def set_generator_limits(self, id: str, pmax_mw: float, pmin_mw: float) -> None:
        """Set generator real power limits (MW)."""
        ...
    def set_generator_reactive_limits(
        self,
        id: str,
        qmin_mvar: float,
        qmax_mvar: float,
    ) -> None:
        """Set generator reactive power limits (MVAr).

        Args:
            id: Generator ID (e.g. ``"gen_1_1"``).
            qmin_mvar: Minimum reactive output (MVAr, typically negative).
            qmax_mvar: Maximum reactive output (MVAr).

        Raises:
            NetworkError: if the generator is not found.
        """
        ...
    def set_generator_setpoint(self, id: str, vs_pu: float) -> None:
        """Set generator voltage setpoint (p.u.)."""
        ...
    def set_generator_voltage_regulated(self, id: str, voltage_regulated: bool) -> None:
        """Set whether a generator participates in AC voltage regulation."""
        ...
    def set_generator_regulated_bus(self, id: str, regulated_bus: int | None) -> None:
        """Set the bus regulated by a generator. ``None`` restores local regulation."""
        ...
    def set_generator_cost(self, id: str, coeffs: list[float]) -> None:
        """Set polynomial cost curve for a generator.

        Parameters
        ----------
        id : str
            Generator ID (e.g. ``"gen_1_1"``).
        coeffs : list[float]
            Polynomial coefficients, highest-order first (e.g. ``[c2, c1, c0]``
            for ``cost = c2*P^2 + c1*P + c0``, in $/hr).
        """
        ...
    def set_generator_reserve_offers(
        self,
        id: str,
        reserve_offers: list[tuple[str, float, float]],
    ) -> None:
        """Replace a generator's reserve offers."""
        ...
    def set_generator_qualifications(
        self,
        id: str,
        qualifications: dict[str, bool],
    ) -> None:
        """Replace a generator's reserve qualification flags."""
        ...
    def add_generator_object(self, generator: Generator) -> None:
        """Add a generator from an editable ``Generator`` object."""
        ...
    def update_generator_object(self, generator: Generator) -> None:
        """Apply an editable ``Generator`` object back onto the network."""
        ...
    def add_load(
        self,
        bus: int,
        pd_mw: float,
        qd_mvar: float = 0.0,
        in_service: bool = True,
        conforming: bool = True,
        load_id: str = "1",
    ) -> None:
        """Add a load at a bus.

        Args:
            bus: External bus number.
            pd_mw: Active power demand (MW).
            qd_mvar: Reactive power demand (MVAr, default 0.0).
            in_service: Whether the load is in service (default True).
            conforming: Conforming load flag (scales with system load, default True).
            load_id: Load identifier string (default ``"1"``).

        Raises:
            NetworkError: if ``bus`` does not exist in the network.
        """
        ...
    def remove_load(self, bus: int, load_id: str = "1") -> None:
        """Remove a load from a bus.

        Raises:
            NetworkError: if the load is not found.
        """
        ...
    def set_load_in_service(
        self, bus: int, in_service: bool, load_id: str = "1"
    ) -> None:
        """Set the in-service status of a load.

        Raises:
            NetworkError: if the load is not found.
        """
        ...
    def add_load_object(self, load: Load) -> None:
        """Add a load from an editable ``Load`` object."""
        ...
    def update_load_object(self, load: Load) -> None:
        """Apply an editable ``Load`` object back onto the network."""
        ...
    def add_dispatchable_load(
        self,
        bus: int,
        p_sched_mw: float,
        q_sched_mvar: float = 0.0,
        p_min_mw: float = 0.0,
        cost_per_mwh: float = 0.0,
        archetype: str = "Curtailable",
        in_service: bool = True,
        baseline_mw: float | None = None,
        reserve_offers: list[tuple[str, float, float]] | None = None,
        qualifications: dict[str, bool] | None = None,
    ) -> None:
        """Add a dispatchable-load resource to the network.

        Supported archetypes: ``"Curtailable"`` and ``"Interruptible"``.
        """
        ...
    def remove_dispatchable_load(self, index: int) -> None:
        """Remove a dispatchable-load resource by list index."""
        ...
    def set_dispatchable_load_in_service(self, index: int, in_service: bool) -> None:
        """Set a dispatchable-load resource in- or out-of-service by list index."""
        ...
    def set_dispatchable_load_reserve_offers(
        self,
        index: int,
        reserve_offers: list[tuple[str, float, float]],
    ) -> None:
        """Replace reserve offers on a dispatchable-load resource by list index."""
        ...
    def set_dispatchable_load_qualifications(
        self,
        index: int,
        qualifications: dict[str, bool],
    ) -> None:
        """Replace reserve qualification flags on a dispatchable-load resource by list index."""
        ...
    def add_dispatchable_load_object(self, load: DispatchableLoad) -> None:
        """Add a dispatchable-load resource from an editable ``DispatchableLoad`` object."""
        ...
    def update_dispatchable_load_object(self, load: DispatchableLoad) -> None:
        """Apply an editable ``DispatchableLoad`` object back onto the network."""
        ...
    def copy(self) -> "Network":
        """Return an independent deep copy of this network."""
        ...
    def apply_voltages(
        self,
        vm_pu: list[float],
        va_deg: list[float],
        bus_numbers: list[int],
    ) -> None:
        """Apply solved voltage magnitudes and angles to the network buses.

        Args:
            vm_pu: Voltage magnitudes in per-unit.
            va_deg: Voltage angles in degrees.
            bus_numbers: External bus numbers (same order as vm_pu / va_deg).
        """
        ...
    # ── Interface / Flowgate operations ────────────────────────────────────
    def add_interface(
        self,
        name: str,
        members: list[tuple[tuple[int, int, int | str], float]],
        limit_forward_mw: float,
        limit_reverse_mw: float = 0.0,
    ) -> None:
        """Add a transmission interface (a set of branches defining a flow boundary).

        Parameters
        ----------
        name : str
            Interface name (e.g. ``"Houston Import"``).
        members : list[tuple[tuple[int, int, int | str], float]]
            List of ``((from_bus, to_bus, circuit), coefficient)`` entries.
        limit_forward_mw : float
            MW limit in the forward direction.
        limit_reverse_mw : float, optional
            MW limit in the reverse direction (positive magnitude, default 0.0).

        Raises
        ------
        ValueError
            If a referenced branch does not exist in the network.
        """
        ...
    def remove_interface(self, name: str) -> None:
        """Remove a transmission interface by name.

        Raises
        ------
        ValueError
            If no interface with the given name exists.
        """
        ...
    def add_flowgate(
        self,
        name: str,
        monitored: list[tuple[tuple[int, int, int | str], float]],
        limit_mw: float,
        contingency_branch: Optional[tuple[int, int, int | str]] = None,
    ) -> None:
        """Add a flowgate (a monitored element under a specific contingency).

        Parameters
        ----------
        name : str
            Flowgate name (e.g. ``"FG_123"``).
        monitored : list[tuple[tuple[int, int, int | str], float]]
            List of ``((from_bus, to_bus, circuit), coefficient)`` entries for the
            monitored elements.
        limit_mw : float
            MW limit.
        contingency_branch : tuple[int, int, int | str] or None, optional
            ``(from_bus, to_bus, circuit)`` of the contingency element, or ``None``
            for a base-case-only flowgate (default ``None``).

        Raises
        ------
        ValueError
            If a referenced branch does not exist in the network.
        """
        ...
    def remove_flowgate(self, name: str) -> None:
        """Remove a flowgate by name.

        Raises
        ------
        ValueError
            If no flowgate with the given name exists.
        """
        ...
    def set_zip_load(
        self,
        bus: int,
        pz: float = 0.0,
        pi: float = 0.0,
        pp: float = 100.0,
        qz: float = 0.0,
        qi: float = 0.0,
        qp: float = 100.0,
    ) -> None:
        """Set ZIP load model coefficients for a bus.

        Coefficients are in percent and must sum to 100 for P and Q separately.
        ZIP loads are always active when coefficients are set.

        Args:
            bus: External bus number.
            pz: Constant-impedance P fraction (%, default 0).
            pi: Constant-current P fraction (%, default 0).
            pp: Constant-power P fraction (%, default 100).
            qz: Constant-impedance Q fraction (%, default 0).
            qi: Constant-current Q fraction (%, default 0).
            qp: Constant-power Q fraction (%, default 100).
        """
        ...
    # ── Rich element objects ────────────────────────────────────────────────
    @property
    def buses(self) -> list[Bus]:
        """All buses as ``Bus`` objects (static model data).

        Returns one object per bus in ``network.buses`` order.

        Example::

            for bus in net.buses:
                print(bus.number, bus.base_kv, bus.pd_mw)
        """
        ...
    @property
    def branches(self) -> list[Branch]:
        """All branches as ``Branch`` objects (static model data).

        Example::

            transformers = [br for br in net.branches if br.is_transformer]
        """
        ...
    @property
    def generators(self) -> list[Generator]:
        """All generators as ``Generator`` objects (static model data).

        Example::

            gas_gens = [g for g in net.generators if g.fuel_type == 'gas']
        """
        ...
    @property
    def loads(self) -> list[Load]:
        """All explicit Load records as ``Load`` objects.

        Note: MATPOWER-format networks embed load in bus ``pd``/``qd``; this
        list is empty for those cases. PSS/E-format networks with LOAD records
        will have non-empty lists.
        """
        ...
    @property
    def slack_bus(self) -> Bus:
        """The reference (slack) bus as a ``Bus`` object.

        Raises ``ValueError`` if the network has no slack bus.
        """
        ...
    def bus(self, number: int) -> Bus:
        """Return the ``Bus`` with external bus number *number*.

        Raises ``ValueError`` if *number* is not in the network.
        """
        ...
    def branch(self, from_bus: int, to_bus: int, circuit: int | str = "1") -> Branch:
        """Return the ``Branch`` between *from_bus* and *to_bus*.

        Raises ``ValueError`` if the branch is not found.
        """
        ...
    def generator(self, bus: int, machine_id: str = "1") -> Generator:
        """Return the ``Generator`` at *bus* with PSS/E machine ID *machine_id*.

        Raises ``ValueError`` if no matching generator is found.
        """
        ...
    def bus_index(self, number: int) -> int:
        """Return the 0-based internal index of the bus with external number *number*.

        Use this to build index-keyed inputs (SE measurements, derate profiles, etc.)
        from external bus numbers.  Raises ``ValueError`` if not found.

        Example::

            idx = net.bus_index(1234)
            measurements.append({"type": "v_mag", "value": 1.02, "sigma": 0.01, "bus": idx})
        """
        ...
    def branch_index(self, from_bus: int, to_bus: int, circuit: int | str = "1") -> int:
        """Return the 0-based internal index of the branch (from_bus, to_bus, circuit).

        Also checks the reversed direction.  Raises ``ValueError`` if not found.

        Example::

            idx = net.branch_index(1, 4)
            branch_derates = {idx: [1.0, 0.0, 1.0]}  # out of service in hour 1
        """
        ...
    def generator_index(self, bus: int, machine_id: str = "1") -> int:
        """Return the 0-based internal index of the generator at *bus* with *machine_id*.

        Raises ``ValueError`` if not found.

        Example::

            idx = net.generator_index(30)
            gen_derates = {idx: [1.0, 0.5, 0.0]}  # partial derate h1, outage h2
        """
        ...

    # ── Phase E: Network collection helpers ─────────────────────────────────
    @property
    def transformers(self) -> list[Branch]:
        """All transformer branches (is_transformer == True)."""
        ...
    @property
    def lines(self) -> list[Branch]:
        """All line branches (is_transformer == False)."""
        ...
    @property
    def in_service_generators(self) -> list[Generator]:
        """All in-service generators."""
        ...
    @property
    def in_service_branches(self) -> list[Branch]:
        """All in-service branches."""
        ...
    def generators_at_bus(self, bus: int) -> list[Generator]:
        """All generators connected to the given bus number."""
        ...
    def branches_at_bus(self, bus: int) -> list[Branch]:
        """All branches incident to the given bus (from or to end)."""
        ...
    def loads_at_bus(self, bus: int) -> list[Load]:
        """All explicit load records at the given bus number."""
        ...
    @property
    def area_numbers(self) -> list[int]:
        """Sorted unique area numbers across all buses."""
        ...
    @property
    def zone_numbers(self) -> list[int]:
        """Sorted unique zone numbers across all buses."""
        ...
    @property
    def total_load_mvar(self) -> float:
        """Total reactive power load from bus Qd fields (MVAr)."""
        ...
    @property
    def total_scheduled_generation_mw(self) -> float:
        """Total scheduled generation from in-service generators (MW)."""
        ...
    @property
    def total_scheduled_generation_mvar(self) -> float:
        """Total scheduled reactive generation from in-service generators (MVAr)."""
        ...
    @property
    def generation_reserve_mw(self) -> float:
        """Sum of (pmax - pg) for all in-service generators (MW)."""
        ...
    @property
    def hvdc(self) -> Hvdc:
        """Canonical HVDC namespace for point-to-point links and explicit DC topology."""
        ...
    @property
    def dispatchable_loads(self) -> list[DispatchableLoad]:
        """All dispatchable load (demand response) resources."""
        ...
    @property
    def facts_devices(self) -> list[FactsDevice]:
        """All FACTS devices (SVCs, STATCOMs, TCSCs, UPFCs)."""
        ...
    @property
    def area_schedules(self) -> list[AreaSchedule]:
        """All area interchange control records."""
        ...
    @property
    def pumped_hydro_units(self) -> list[PumpedHydroUnit]:
        """All pumped hydro units as editable objects."""
        ...
    @property
    def breaker_ratings(self) -> list[BreakerRating]:
        """All breaker ratings as editable objects."""
        ...
    @property
    def fixed_shunts(self) -> list[FixedShunt]:
        """All fixed shunts as editable objects."""
        ...
    @property
    def combined_cycle_plants(self) -> list[CombinedCyclePlant]:
        """All combined cycle plants as editable objects."""
        ...
    @property
    def outage_entries(self) -> list[OutageEntry]:
        """All outage schedule rows as editable objects."""
        ...
    @property
    def reserve_zones(self) -> list[ReserveZone]:
        """All reserve zones as editable objects."""
        ...
    # ── DC lines ────────────────────────────────────────────────────────────
    def add_lcc_dc_line(
        self,
        name: str,
        rect_bus: int,
        inv_bus: int,
        setvl_mw: float,
        vschd_kv: float = 500.0,
        rdc: float = 0.0,
        p_dc_min_mw: float = 0.0,
        p_dc_max_mw: float = 0.0,
    ) -> None:
        """Add an LCC (classical) HVDC line.

        When ``p_dc_min_mw < p_dc_max_mw`` the joint AC-DC OPF treats this
        link's DC power as an NLP decision variable bounded by the range;
        otherwise the link is pinned at ``setvl_mw`` and the sequential
        AC-DC iteration handles it.

        Args:
            name: Unique name for the DC link.
            rect_bus: Rectifier (sending) bus number.
            inv_bus: Inverter (receiving) bus number.
            setvl_mw: Scheduled DC power flow (MW).
            vschd_kv: Scheduled DC voltage (kV, default 500).
            rdc: DC resistance (ohm, default 0).
            p_dc_min_mw: Minimum DC power for joint AC-DC OPF (MW, default 0).
            p_dc_max_mw: Maximum DC power for joint AC-DC OPF (MW, default 0).

        Raises:
            NetworkError: if either bus does not exist, or p_dc_min_mw > p_dc_max_mw.
        """
        ...
    def remove_lcc_dc_line(
        self, rectifier_bus: int, inverter_bus: int
    ) -> None:
        """Remove an LCC HVDC line by its terminal buses.

        Raises:
            NetworkError: if the DC line is not found.
        """
        ...
    def add_dc_line_object(self, line: LccHvdcLink) -> None:
        """Add an LCC-HVDC line from an editable ``LccHvdcLink`` object."""
        ...
    def update_dc_line_object(self, line: LccHvdcLink) -> None:
        """Apply an editable ``LccHvdcLink`` object back onto the network."""
        ...
    def add_vsc_dc_line(
        self,
        bus1: int,
        bus2: int,
        p_set_mw: float,
        mode1: str = "PF",
        mode2: str = "PF",
    ) -> None:
        """Add a VSC-HVDC line.

        Args:
            bus1: Converter-1 AC bus number.
            bus2: Converter-2 AC bus number.
            p_set_mw: Active power transfer setpoint (MW, positive = bus1→bus2).
            mode1: Converter-1 control mode: ``'PF'`` (power flow, default),
                ``'DC_VOLTAGE'``, or ``'SLACK'``.
            mode2: Converter-2 control mode (same options, default ``'PF'``).

        Raises:
            NetworkError: if either bus does not exist.
        """
        ...
    def remove_vsc_dc_line(self, bus1: int, bus2: int) -> None:
        """Remove a VSC-HVDC line by its terminal buses.

        Raises:
            NetworkError: if the VSC DC line is not found.
        """
        ...
    def add_vsc_dc_line_object(self, line: VscHvdcLink) -> None:
        """Add a VSC-HVDC line from an editable ``VscHvdcLink`` object."""
        ...
    def update_vsc_dc_line_object(self, line: VscHvdcLink) -> None:
        """Apply an editable ``VscHvdcLink`` object back onto the network."""
        ...
    # ── FACTS devices ───────────────────────────────────────────────────────
    def add_facts_device(
        self,
        name: str,
        bus_from: int,
        bus_to: int = 0,
        mode: str = "ShuntOnly",
        v_set: float = 1.0,
        q_max: float = 9999.0,
        linx: float = 0.0,
    ) -> None:
        """Add a FACTS device to the network.

        Args:
            name: Unique FACTS device name.
            bus_from: Shunt connection bus number.
            bus_to: Series/remote bus number (0 for shunt-only devices).
            mode: Operating mode string.
            v_set: Voltage setpoint at ``bus_from`` in per-unit.
            q_max: Maximum reactive injection magnitude in MVAr.
            linx: Series reactance contribution in per-unit.

        Raises:
            NetworkError: if ``bus_from`` or ``bus_to`` does not exist.
        """
        ...
    def remove_facts_device(self, name: str) -> None:
        """Remove a FACTS device by name.

        Raises:
            NetworkError: if the device is not found.
        """
        ...
    def add_facts_device_object(self, device: FactsDevice) -> None:
        """Add a FACTS device from an editable ``FactsDevice`` object."""
        ...
    def update_facts_device_object(self, device: FactsDevice) -> None:
        """Apply an editable ``FactsDevice`` object back onto the network."""
        ...
    def add_area_schedule_object(self, schedule: AreaSchedule) -> None:
        """Add an area interchange record from an editable ``AreaSchedule`` object."""
        ...
    def update_area_schedule_object(self, schedule: AreaSchedule) -> None:
        """Apply an editable ``AreaSchedule`` object back onto the network."""
        ...
    def add_breaker_rating_object(self, rating: BreakerRating) -> None:
        """Add a breaker rating from an editable ``BreakerRating`` object."""
        ...
    def update_breaker_rating_object(self, rating: BreakerRating) -> None:
        """Apply an editable ``BreakerRating`` object back onto the network."""
        ...
    def add_fixed_shunt_object(self, shunt: FixedShunt) -> None:
        """Add a fixed shunt from an editable ``FixedShunt`` object."""
        ...
    def update_fixed_shunt_object(self, shunt: FixedShunt) -> None:
        """Apply an editable ``FixedShunt`` object back onto the network."""
        ...
    def add_reserve_zone_object(self, zone: ReserveZone) -> None:
        """Add a reserve zone from an editable ``ReserveZone`` object."""
        ...
    def update_reserve_zone_object(self, zone: ReserveZone) -> None:
        """Apply an editable ``ReserveZone`` object back onto the network."""
        ...
    def add_pumped_hydro_unit_object(self, unit: PumpedHydroUnit) -> None:
        """Add a pumped-hydro unit from an editable ``PumpedHydroUnit`` object."""
        ...
    def update_pumped_hydro_unit_object(self, unit: PumpedHydroUnit) -> None:
        """Apply an editable ``PumpedHydroUnit`` object back onto the network."""
        ...
    def add_combined_cycle_plant_object(self, plant: CombinedCyclePlant) -> None:
        """Add a combined-cycle plant from an editable ``CombinedCyclePlant`` object."""
        ...
    def update_combined_cycle_plant_object(self, plant: CombinedCyclePlant) -> None:
        """Apply an editable ``CombinedCyclePlant`` object back onto the network."""
        ...
    def add_outage_entry_object(self, outage: OutageEntry) -> None:
        """Add an outage record from an editable ``OutageEntry`` object."""
        ...
    def update_outage_entry_object(self, outage: OutageEntry) -> None:
        """Apply an editable ``OutageEntry`` object back onto the network."""
        ...
    def set_facts_mode(self, name: str, mode: str) -> None:
        """Change the operating mode of a FACTS device.

        Args:
            name: FACTS device name.
            mode: New mode: ``'SVC'``, ``'STATCOM'``, ``'TCSC'``, or ``'UPFC'``.

        Raises:
            NetworkError: if the device is not found.
        """
        ...


class DcPfResult:
    """DC power flow solution.

    Returned by ``solve_dc_pf()``.
    """

    @property
    def va_rad(self) -> NDArray[np.float64]:
        """Bus voltage angles in radians as numpy array."""
        ...
    @property
    def va_deg(self) -> NDArray[np.float64]:
        """Bus voltage angles in degrees as numpy array."""
        ...
    @property
    def branch_p_mw(self) -> NDArray[np.float64]:
        """Branch active power flows (MW) as numpy array."""
        ...
    @property
    def slack_p_mw(self) -> float:
        """Slack bus real power injection (MW)."""
        ...
    @property
    def solve_time_secs(self) -> float:
        """Solve time in seconds."""
        ...
    @property
    def total_generation_mw(self) -> float:
        """Total system generation after slack balancing (MW)."""
        ...
    @property
    def slack_distribution_mw(self) -> dict[int, float]:
        """Per-bus slack distribution (bus number to MW share).

        Non-empty only when headroom slack is used.
        """
        ...
    @property
    def bus_p_inject_mw(self) -> NDArray[np.float64]:
        """Net real power injection at each bus (MW)."""
        ...
    @property
    def bus_numbers(self) -> list[int]:
        """External bus numbers in bus order."""
        ...
    @property
    def branch_from(self) -> list[int]:
        """External from-bus numbers in branch order."""
        ...
    @property
    def branch_to(self) -> list[int]:
        """External to-bus numbers in branch order."""
        ...
    @property
    def branch_circuit(self) -> list[str]:
        """Circuit identifiers in branch order."""
        ...
    @property
    def branch_keys(self) -> list[tuple[int, int, str]]:
        """Stable branch keys in branch order."""
        ...
    def to_dataframe(self) -> Any:
        """Return bus-level DataFrame: bus_id, va_rad, va_deg."""
        ...
    def branch_dataframe(self) -> Any:
        """Return branch-level DataFrame: from_bus, to_bus, circuit, p_mw."""
        ...
    @property
    def buses(self) -> list[BusDcSolved]:
        """Return a list of ``BusDcSolved`` objects with DC power flow results."""
        ...
    @property
    def branches(self) -> list[BranchDcSolved]:
        """Return a list of ``BranchDcSolved`` objects with DC power flow results."""
        ...
    def __repr__(self) -> str: ...


class BusDcSolved:
    """Bus with DC power flow results."""

    @property
    def number(self) -> int: ...
    @property
    def name(self) -> str: ...
    @property
    def type_str(self) -> str: ...
    @property
    def pd_mw(self) -> float: ...
    @property
    def qd_mvar(self) -> float: ...
    @property
    def area(self) -> int: ...
    @property
    def zone(self) -> int: ...
    @property
    def base_kv(self) -> float: ...
    @property
    def theta_rad(self) -> float:
        """Solved voltage angle (radians)."""
        ...
    @property
    def theta_deg(self) -> float:
        """Solved voltage angle (degrees)."""
        ...
    def __repr__(self) -> str: ...


class BranchDcSolved:
    """Branch with DC power flow results."""

    @property
    def from_bus(self) -> int: ...
    @property
    def to_bus(self) -> int: ...
    @property
    def circuit(self) -> str: ...
    @property
    def r_pu(self) -> float: ...
    @property
    def x_pu(self) -> float: ...
    @property
    def b_pu(self) -> float: ...
    @property
    def rate_a_mva(self) -> float: ...
    @property
    def tap(self) -> float: ...
    @property
    def shift_deg(self) -> float: ...
    @property
    def in_service(self) -> bool: ...
    @property
    def flow_mw(self) -> float:
        """Active power flow (MW)."""
        ...
    @property
    def loading_pct(self) -> float:
        """Branch loading as % of Rate A."""
        ...
    def __repr__(self) -> str: ...


class AcPfResult:
    """AC power flow solution (Newton-Raphson or Fast Decoupled)."""

    @property
    def converged(self) -> bool: ...
    @property
    def status(self) -> str:
        """Solver status string: ``'Converged'``, ``'MaxIterations'``, ``'Diverged'``, or ``'Unsolved'``."""
        ...
    @property
    def iterations(self) -> int | None: ...
    @property
    def max_mismatch(self) -> float:
        """Maximum power mismatch at convergence (p.u.)."""
        ...
    @property
    def solve_time_secs(self) -> float: ...
    @property
    def convergence_history(self) -> NDArray[np.float64]:
        """Per-iteration convergence data as Nx2 array [iteration, max_mismatch_pu].

        Empty (0x2) array if record_convergence_history was not enabled.
        """
        ...
    @property
    def vm(self) -> NDArray[np.float64]:
        """Bus voltage magnitudes (p.u.) as a 1-D numpy array."""
        ...
    @property
    def va_rad(self) -> NDArray[np.float64]:
        """Bus voltage angles in radians as a 1-D numpy array."""
        ...
    @property
    def va_deg(self) -> NDArray[np.float64]:
        """Bus voltage angles in degrees as a 1-D numpy array."""
        ...
    @property
    def p_inject_mw(self) -> NDArray[np.float64]:
        """Active power injections (MW) as a 1-D numpy array."""
        ...
    @property
    def q_inject_mvar(self) -> NDArray[np.float64]:
        """Reactive power injections (MVAr) as a 1-D numpy array."""
        ...
    @property
    def area_interchange(self) -> dict | None:
        """Area interchange enforcement results, or ``None`` if not enabled.

        Returns a dict with keys:

        * ``converged`` (bool) — whether all areas met their targets.
        * ``iterations`` (int) — outer-loop iterations used.
        * ``areas`` — list of dicts with ``area``, ``scheduled_mw``,
          ``actual_mw``, ``error_mw``, ``dispatch_method``.
        """
        ...
    def branch_apparent_power(self) -> NDArray[np.float64]:
        """Branch apparent power flows (MVA) as a 1-D numpy array."""
        ...
    def branch_loading_pct(self) -> NDArray[np.float64]:
        """Branch loading percentage (% of rating) as a 1-D numpy array."""
        ...
    # --- Phase 4: Per-generator Q output ---
    @property
    def gen_q_mvar(self) -> NDArray[np.float64]:
        """Reactive power output (MVAr) per generator, apportioned from bus Q injection.

        Indexed by network.generators order. Populated for converged solutions.
        """
        ...
    @property
    def q_limited_buses(self) -> list[int]:
        """External bus numbers of buses that hit Q limits (PV→PQ switches)."""
        ...
    @property
    def n_q_limit_switches(self) -> int:
        """Total number of PV→PQ or PQ→PV bus type switches during this solve."""
        ...
    @property
    def island_ids(self) -> list[int]:
        """Per-bus island assignment (0-indexed) when island detection was run.

        Empty when detect_islands was not enabled. island_ids[i] gives the
        connected-component index for internal bus i.
        """
        ...
    @property
    def n_islands(self) -> int:
        """Number of distinct islands. 0 when island detection was not performed."""
        ...
    def to_dataframe(self) -> pd.DataFrame:
        """Return dict for pd.DataFrame. Columns: bus_id, vm_pu, va_deg, p_mw, q_mvar."""
        ...
    # ── Rich element objects ────────────────────────────────────────────────
    def get_buses(self) -> list[BusSolved]:
        """All buses merged with power flow results.

        Example::

            for b in sol.buses:
                print(b.number, b.vm_pu, b.va_deg, b.p_inject_mw)
        """
        ...
    def bus(self, number: int) -> BusSolved:
        """A single bus by external number, merged with power flow results.

        Raises ``ValueError`` if *number* is not in the network.
        """
        ...
    def get_branches(self) -> list[BranchSolved]:
        """All branches with power flow result flows.

        Example::

            overloaded = [b for b in sol.branches if b.loading_pct > 90]
        """
        ...
    def get_generators(self) -> list[GenSolved]:
        """All generators with solved reactive power output.

        Example::

            for g in sol.generators:
                print(g.bus, g.p_mw, g.q_mvar_solved)
        """
        ...
    def validate(self) -> bool:
        """Validate solution data. Returns True if valid, raises ValueError if not."""
        ...
    # ── Phase F: Solution query helpers ─────────────────────────────────────
    def violated_buses(
        self,
        vmin: float = 0.95,
        vmax: float = 1.05,
    ) -> list[BusSolved]:
        """Return buses with solved voltage outside [vmin, vmax] p.u.

        Example::

            low_v = sol.violated_buses(vmin=0.95, vmax=1.05)
        """
        ...
    def overloaded_branches(
        self,
        threshold_pct: float = 100.0,
    ) -> list[BranchSolved]:
        """Return branches with loading_pct > threshold_pct (default 100%).

        Example::

            overloaded = sol.overloaded_branches(threshold_pct=90.0)
        """
        ...
    def to_json(self) -> str:
        """Serialize this solution to a JSON string.

        The JSON can be stored to disk and later restored with ``from_json()``.

        Example::

            json_str = sol.to_json()
            with open("solution.json", "w") as f:
                f.write(json_str)
        """
        ...
    @staticmethod
    def from_json(s: str) -> "AcPfResult":
        """Deserialize a ``AcPfResult`` from a JSON string.

        Args:
            s: JSON string produced by ``to_json()``.

        Raises:
            ValueError: if the JSON is malformed or missing required fields.

        Example::

            with open("solution.json") as f:
                sol = AcPfResult.from_json(f.read())
        """
        ...
    def to_dict(self) -> dict[str, object]:
        """Return the solution as a plain Python dict.

        Keys mirror the JSON representation: ``converged``, ``iterations``,
        ``max_mismatch``, ``vm``, ``va_rad``, ``p_inject_mw``, etc.

        Example::

            d = sol.to_dict()
            import json
            print(json.dumps(d, indent=2))
        """
        ...
    def __repr__(self) -> str: ...


class OpfResult:
    """Base OPF solution surface shared by DC-OPF and AC-OPF results."""

    @property
    def total_cost(self) -> float:
        """Total generation cost ($/hr)."""
        ...
    @property
    def opf_type(self) -> str:
        """OPF formulation type: 'dc_opf', 'ac_opf', 'dc_scopf', 'ac_scopf', 'hvdc_opf'."""
        ...
    @property
    def base_mva(self) -> float:
        """System MVA base used by the solve."""
        ...
    @property
    def solve_time_secs(self) -> float: ...
    @property
    def iterations(self) -> int: ...
    @property
    def gen_p_mw(self) -> NDArray[np.float64]:
        """Generator active power dispatch (MW) as a 1-D numpy array."""
        ...
    @property
    def gen_bus_numbers(self) -> list[int]:
        """External bus number for each entry in gen_p_mw / gen_q_mvar (in-service generator order)."""
        ...
    @property
    def gen_ids(self) -> list[str]:
        """Canonical generator ID for each entry in gen_p_mw / gen_q_mvar."""
        ...
    @property
    def gen_machine_ids(self) -> list[str]:
        """Machine ID for each entry in gen_p_mw / gen_q_mvar."""
        ...
    @property
    def lmp(self) -> NDArray[np.float64]:
        """Locational marginal prices ($/MWh) as a 1-D numpy array."""
        ...
    @property
    def lmp_congestion(self) -> NDArray[np.float64]:
        """LMP congestion component ($/MWh) as a 1-D numpy array."""
        ...
    @property
    def lmp_loss(self) -> NDArray[np.float64]:
        """LMP loss component ($/MWh) as a 1-D numpy array."""
        ...
    @property
    def vm(self) -> NDArray[np.float64]:
        """Bus voltage magnitudes (p.u.) as a 1-D numpy array.

        DC-OPF: flat (all 1.0) — no voltage variables in DC formulation.
        AC-OPF: optimal voltages from the NLP solution.
        """
        ...
    @property
    def va_rad(self) -> NDArray[np.float64]:
        """Bus voltage angles in radians as a 1-D numpy array.

        DC-OPF: optimal angles from the B-theta formulation.
        AC-OPF: optimal angles from the NLP solution.
        """
        ...
    @property
    def branch_shadow_prices(self) -> NDArray[np.float64]:
        """Branch shadow prices ($/MWh) as a 1-D numpy array."""
        ...
    @property
    def gen_q_mvar(self) -> NDArray[np.float64]:
        """Generator reactive power dispatch (MVAr) as a 1-D numpy array.

        AC-OPF: optimal reactive power from the NLP solution.
        DC-OPF: empty array (no reactive variables in DC formulation).
        """
        ...
    @property
    def total_load_mw(self) -> float:
        """Total system load (MW)."""
        ...
    @property
    def total_generation_mw(self) -> float:
        """Total generation (MW) — sum of all in-service generator dispatches."""
        ...
    @property
    def total_losses_mw(self) -> float:
        """Total system losses (MW) — total_generation_mw minus total_load_mw.

        Zero for DC-OPF (lossless). Non-zero for AC-OPF.
        """
        ...
    @property
    def lmp_energy(self) -> NDArray[np.float64]:
        """Energy component of LMP per bus ($/MWh).

        Uniform for DC-OPF (equals the slack bus price). For AC-OPF: reference
        bus price. Decomposition: lmp = lmp_energy + lmp_congestion + lmp_loss.
        """
        ...
    @property
    def lmp_reactive(self) -> NDArray[np.float64]:
        """Reactive LMP per bus ($/MVAr-h) as a 1-D numpy array.

        Derived from the Q-balance constraint KKT multiplier at each bus:
            lmp_reactive[i] = lambda_Q[i] / base_mva

        Positive value: load pays for reactive supply at that bus.
        Non-zero values indicate the network is reactive-power constrained
        (generator qmin/qmax limit binding, voltage limit, or branch reactive
        flow limit). Use to identify buses needing reactive compensation.

        AC-OPF only. Empty array for DC-OPF (no reactive variables).
        """
        ...
    @property
    def mu_pg_min(self) -> NDArray[np.float64]:
        """Lower active-power bound duals ($/MWh), one per in-service generator.

        mu_pg_min[j] > 0 means generator j is at its pmin limit.
        KKT: the LMP at generator j's bus equals
        marginal_cost[j] + mu_pg_max[j] - mu_pg_min[j].
        Empty for DC-OPF (column duals not extracted by default).
        """
        ...
    @property
    def mu_pg_max(self) -> NDArray[np.float64]:
        """Upper active-power bound duals ($/MWh), one per in-service generator.

        mu_pg_max[j] > 0 means generator j is at its pmax limit.
        """
        ...
    @property
    def mu_qg_min(self) -> NDArray[np.float64]:
        """Reactive power lower-bound duals ($/MWh), one per in-service generator.

        mu_qg_min[j] > 0 means generator j is at its qmin limit.
        AC-OPF only. Empty array for DC-OPF.
        """
        ...
    @property
    def mu_qg_max(self) -> NDArray[np.float64]:
        """Reactive power upper-bound duals ($/MWh), one per in-service generator.

        mu_qg_max[j] > 0 means generator j is at its qmax limit and is a
        reactive bottleneck — its bus lmp_reactive will be elevated.
        AC-OPF only. Empty array for DC-OPF.
        """
        ...
    @property
    def mu_vm_min(self) -> NDArray[np.float64]:
        """Voltage magnitude lower-bound duals ($/MWh per p.u.), one per bus.

        mu_vm_min[i] > 0 means bus i is at its vmin limit.
        AC-OPF only. Empty array for DC-OPF.
        """
        ...
    @property
    def mu_vm_max(self) -> NDArray[np.float64]:
        """Voltage magnitude upper-bound duals ($/MWh per p.u.), one per bus.

        mu_vm_max[i] > 0 means bus i is at its vmax limit.
        AC-OPF only. Empty array for DC-OPF.
        """
        ...
    @property
    def branch_pf_mw(self) -> NDArray[np.float64]:
        """From-end active power flow per branch (MW).

        DC-OPF: computed from B-theta optimal angles.
        AC-OPF: from the complex voltage solution.
        """
        ...
    @property
    def branch_pt_mw(self) -> NDArray[np.float64]:
        """To-end active power flow per branch (MW).

        DC-OPF (lossless): equals -branch_pf_mw. AC-OPF: differs due to losses.
        """
        ...
    @property
    def branch_qf_mvar(self) -> NDArray[np.float64]:
        """From-end reactive power flow per branch (MVAr). AC-OPF only."""
        ...
    @property
    def branch_qt_mvar(self) -> NDArray[np.float64]:
        """To-end reactive power flow per branch (MVAr). AC-OPF only."""
        ...
    @property
    def branch_loading_pct(self) -> NDArray[np.float64]:
        """Branch loading as a percentage of Rate A thermal limit.

        DC: |Pf| / rate_a * 100. AC: |Sf| = sqrt(Pf^2 + Qf^2) / rate_a * 100.
        NaN where the branch has no positive Rate A limit. JSON serializes those
        unavailable entries as ``null``.
        """
        ...
    @property
    def binding_branch_indices(self) -> list[int]:
        """Indices of branches with |branch_shadow_prices[i]| > 1e-6 (binding thermal limits)."""
        ...
    @property
    def va_deg(self) -> NDArray[np.float64]:
        """Bus voltage angles in degrees as a 1-D numpy array."""
        ...
    @property
    def mu_angmin(self) -> NDArray[np.float64]:
        """Lower branch angle-difference bound duals, one per branch."""
        ...
    @property
    def mu_angmax(self) -> NDArray[np.float64]:
        """Upper branch angle-difference bound duals, one per branch."""
        ...
    @property
    def tap_dispatch(self) -> list[tuple[int, float, float]]:
        """Transformer tap dispatch: list of (branch_idx, continuous_tap, rounded_tap).

        Non-empty only when AC-OPF is run with optimize_taps=True.
        In Continuous mode, continuous_tap == rounded_tap.
        In RoundAndCheck mode, rounded_tap is the nearest discrete step.
        """
        ...
    @property
    def phase_dispatch(self) -> list[tuple[int, float, float]]:
        """Phase shifter dispatch: list of (branch_idx, continuous_rad, rounded_rad).

        Non-empty only when AC-OPF is run with optimize_phase_shifters=True.
        In Continuous mode, continuous_rad == rounded_rad.
        In RoundAndCheck mode, rounded_rad is the nearest discrete step.
        """
        ...
    @property
    def svc_dispatch(self) -> list[tuple[int, float, float, float]]:
        """SVC dispatch: list of (bus_idx, b_svc_pu, q_inject_mvar, v_bus_pu).

        Non-empty only when AC-OPF is run with optimize_svc=True.
        """
        ...
    @property
    def tcsc_dispatch(self) -> list[tuple[int, float, float, float]]:
        """TCSC dispatch: list of (branch_idx, x_comp_pu, x_eff_pu, p_flow_mw).

        Non-empty only when AC-OPF is run with optimize_tcsc=True.
        """
        ...
    @property
    def discrete_feasible(self) -> Optional[bool]:
        """Whether the discrete operating point is feasible after rounding.

        None: continuous mode (no rounding performed).
        True: round-and-check passed — AC power flow converged with no violations.
        False: round-and-check found violations (see discrete_violations).
        """
        ...
    @property
    def discrete_violations(self) -> list[str]:
        """Human-readable descriptions of violations found during round-and-check.

        Empty when discrete_feasible is None or True.
        """
        ...
    @property
    def storage_net_mw(self) -> NDArray[np.float64]:
        """Net storage dispatch (MW), positive for discharge and negative for charge."""
        ...
    @property
    def par_results(self) -> list[dict]:
        """PAR implied-shift results with from_bus, to_bus, circuit, target_mw, implied_shift_deg, within_limits."""
        ...
    @property
    def benders_cut_duals(self) -> NDArray[np.float64]:
        """Benders-cut duals from AC-SCOPF."""
        ...
    def to_dataframe(self) -> pd.DataFrame:
        """Return dict for pd.DataFrame. Columns: bus_id, lmp, lmp_congestion, lmp_loss."""
        ...
    def to_gen_dataframe(self) -> pd.DataFrame:
        """Return a pandas DataFrame of generator dispatch.

        Index: MultiIndex ``(bus_id, machine_id)``.
        Columns: gen_idx, gen_p_mw.
        """
        ...
    # ── Rich element objects ────────────────────────────────────────────────
    def get_buses(self) -> list[BusOpf]:
        """All buses with OPF LMPs, solved voltage, and shadow prices.

        Example::

            for b in result.buses:
                print(b.number, b.lmp, b.lmp_congestion)

            congested = [b for b in result.buses if abs(b.lmp_congestion) > 1.0]
        """
        ...
    def get_branches(self) -> list[BranchOpf]:
        """All branches with OPF flows and thermal shadow prices.

        Example::

            binding = [br for br in result.branches if br.is_binding]
        """
        ...
    def get_generators(self) -> list[GenOpf]:
        """All generators with OPF dispatch, reactive output, and KKT duals.

        Example::

            for g in result.generators:
                print(g.bus, g.p_mw, g.mu_pmax, g.cost_actual)
        """
        ...
    def lmp_dataframe(self) -> pd.DataFrame:
        """Return a pandas DataFrame of LMPs (or dict if pandas is not installed).

        Columns: ``bus_id``, ``bus_name``, ``lmp``, ``lmp_energy``,
        ``lmp_congestion``, ``lmp_loss``.

        Example::

            import pandas as pd
            df = pd.DataFrame(result.lmp_dataframe())
        """
        ...
    # ── Phase F: Solution query helpers ─────────────────────────────────────
    def binding_branches(
        self,
        threshold: float = 1e-6,
    ) -> list[BranchOpf]:
        """Return branches with |shadow_price| > threshold (binding thermal limits).

        Example::

            congested = result.binding_branches()
        """
        ...
    def congested_buses(
        self,
        threshold: float = 1e-3,
    ) -> list[BusOpf]:
        """Return buses with |lmp_congestion| > threshold (congested).

        Example::

            congested = result.congested_buses()
        """
        ...
    def switched_shunts(self) -> list[SwitchedShuntOpf]:
        """Return OPF-dispatched switched shunt devices.

        Example::

            shunts = result.switched_shunts()
        """
        ...
    @property
    def converged(self) -> bool:
        """True if the OPF solver converged to an optimal solution."""
        ...
    @property
    def solver_name(self) -> str | None:
        """Name of the LP/NLP solver used (e.g. ``'HiGHS'``, ``'Gurobi'``, ``'Ipopt'``).

        ``None`` when the solver identity was not recorded.
        """
        ...
    @property
    def solver_version(self) -> str | None:
        """Version string of the solver (e.g. ``'1.13.1'`` for HiGHS 1.13.1).

        ``None`` when the version was not recorded.
        """
        ...
    @property
    def flowgate_shadow_prices(self) -> dict[str, float]:
        """Flowgate shadow prices ($/MWh) as a dict mapping flowgate name → $/MWh.

        Keys are base-case flowgate names (``contingency_branch = None``).
        Non-zero indicates the flowgate was binding at the OPF optimum.
        Empty for AC-OPF or when no flowgates are defined.
        """
        ...
    @property
    def interface_shadow_prices(self) -> dict[str, float]:
        """Interface shadow prices ($/MWh) as a dict mapping interface name → $/MWh.

        Non-zero indicates the interface limit was binding at the OPF optimum.
        Empty for AC-OPF or when no interfaces are defined.
        """
        ...
    @property
    def has_attached_network(self) -> bool:
        """Whether rich topology-dependent helpers can access an attached Network."""
        ...
    def attach_network(self, network: Network) -> None:
        """Attach a Network to a detached result restored from JSON."""
        ...
    def to_json(self) -> str:
        """Serialize this OPF solution to a JSON string.

        The JSON can be stored to disk and later restored with ``from_json()``.

        Example::

            json_str = result.to_json()
            with open("opf_result.json", "w") as f:
                f.write(json_str)
        """
        ...
    @staticmethod
    def from_json(s: str) -> "OpfResult":
        """Deserialize an ``OpfResult`` from a JSON string.

        Args:
            s: JSON string produced by ``to_json()``.

        Raises:
            ValueError: if the JSON is malformed or missing required fields.

        Example::

            with open("opf_result.json") as f:
                result = OpfResult.from_json(f.read())
        """
        ...
    def to_dict(self) -> dict[str, object]:
        """Return the OPF solution as a plain Python dict.

        Keys mirror the JSON representation: ``total_cost``, ``gen_p_mw``,
        ``lmp``, ``lmp_congestion``, ``lmp_loss``, ``vm``, ``va_rad``, etc.

        Example::

            d = result.to_dict()
            import json
            print(json.dumps(d, indent=2))
        """
        ...

class BindingContingency:
    contingency_label: str
    cut_kind: str
    outaged_branch_indices: list[int]
    outaged_generator_indices: list[int]
    monitored_branch_idx: int
    loading_pct: float
    shadow_price: float


class ContingencyViolation:
    contingency_id: str
    contingency_label: str
    outaged_branches: list[int]
    outaged_generators: list[int]
    thermal_violations: list[tuple[int, float, float, float]]
    voltage_violations: list[tuple[int, float, float, float]]


class ScopfScreeningStats:
    pairs_evaluated: int
    pre_screened_constraints: int
    cutting_plane_constraints: int
    threshold_fraction: float


class FailedContingencyEvaluation:
    contingency_id: str
    contingency_label: str
    outaged_branches: list[int]
    outaged_generators: list[int]
    reason: str


class DcOpfResult:
    @property
    def opf(self) -> OpfResult: ...
    @property
    def hvdc_dispatch_mw(self) -> NDArray[np.float64]: ...
    @property
    def hvdc_shadow_prices(self) -> NDArray[np.float64]: ...
    @property
    def gen_limit_violations(self) -> list[tuple[int, float]]: ...
    @property
    def is_feasible(self) -> bool: ...


class ScopfResult:
    @property
    def base_opf(self) -> OpfResult: ...
    @property
    def formulation(self) -> str: ...
    @property
    def mode(self) -> str: ...
    @property
    def iterations(self) -> int: ...
    @property
    def converged(self) -> bool: ...
    @property
    def total_contingencies_evaluated(self) -> int: ...
    @property
    def total_contingency_constraints(self) -> int: ...
    @property
    def binding_contingencies(self) -> list[BindingContingency]: ...
    @property
    def lmp_contingency_congestion(self) -> NDArray[np.float64]: ...
    @property
    def remaining_violations(self) -> list[ContingencyViolation]: ...
    @property
    def failed_contingencies(self) -> list[FailedContingencyEvaluation]: ...
    @property
    def screening_stats(self) -> ScopfScreeningStats: ...
    @property
    def solve_time_secs(self) -> float: ...


class AcOpfHvdcResult:
    @property
    def opf(self) -> OpfResult: ...
    @property
    def hvdc_p_dc_mw(self) -> NDArray[np.float64]: ...
    @property
    def hvdc_p_loss_mw(self) -> NDArray[np.float64]: ...
    @property
    def hvdc_iterations(self) -> int: ...


class OtsResult:
    converged: bool
    objective: float
    switched_out: list[tuple[int, int, str]]
    n_switches: int
    gen_dispatch: NDArray[np.float64]
    branch_flows: NDArray[np.float64]
    lmps: NDArray[np.float64]
    solve_time_ms: float
    mip_gap: float


class OrpdResult:
    converged: bool
    objective: float
    total_losses_mw: float
    voltage_deviation: float
    vm: NDArray[np.float64]
    va_rad: NDArray[np.float64]
    va_deg: NDArray[np.float64]
    q_dispatch_pu: NDArray[np.float64]
    p_dispatch_pu: NDArray[np.float64]
    q_dispatch_mvar: NDArray[np.float64]
    p_dispatch_mw: NDArray[np.float64]
    iterations: int | None
    solve_time_ms: float


class ReconfigResult:
    open_branches: list[int]
    objective: float
    converged: bool
    solve_time_s: float
    @property
    def virtual_bid_results(self) -> list[dict]:
        """Virtual bid clearing results. Each dict has: bus, direction, cleared_mw, price_per_mwh, lmp.

        Empty when no virtual bids were submitted.
        """
        ...
    def __repr__(self) -> str: ...


class DispatchResult:
    @property
    def study(self) -> dict[str, Any]: ...
    @property
    def resources(self) -> list[dict[str, Any]]: ...
    @property
    def buses(self) -> list[dict[str, Any]]: ...
    @property
    def summary(self) -> dict[str, Any]: ...
    @property
    def diagnostics(self) -> dict[str, Any]: ...
    @property
    def periods(self) -> list[dict[str, Any]]: ...
    @property
    def resource_summaries(self) -> list[dict[str, Any]]: ...
    @property
    def combined_cycle_results(self) -> list[dict[str, Any]]: ...
    def to_json(self) -> str: ...
    @staticmethod
    def from_json(s: str) -> "DispatchResult": ...
    def to_dict(self) -> dict[str, Any]: ...
    def __repr__(self) -> str: ...


class LoleResult:
    """LOLE (Loss of Load Expectation) computation result."""

    @property
    def lole_hours(self) -> float:
        """LOLE in hours/period."""
        ...
    @property
    def lole_days(self) -> float:
        """LOLE in days/period."""
        ...
    @property
    def eue_mwh(self) -> float:
        """Expected unserved energy (MWh/period)."""
        ...
    @property
    def hourly_lolp(self) -> NDArray[np.float64]:
        """Hourly loss-of-load probability as a 1-D numpy array."""
        ...
    @property
    def total_capacity_mw(self) -> float:
        """Total installed capacity (MW)."""
        ...
    @property
    def peak_load_mw(self) -> float:
        """Peak load (MW)."""
        ...
    def to_dataframe(self) -> "pd.DataFrame":
        """Convert hourly LOLP to a DataFrame (columns: hour, lolp)."""
        ...
    def __repr__(self) -> str: ...


class ElccResult:
    """ELCC (Effective Load Carrying Capability) computation result."""

    @property
    def elcc_mw(self) -> float:
        """ELCC in MW."""
        ...
    @property
    def elcc_fraction(self) -> float:
        """ELCC as fraction of nameplate [0, 1]."""
        ...
    @property
    def lole_before(self) -> float:
        """LOLE before adding the resource."""
        ...
    @property
    def lole_after_addition(self) -> float:
        """LOLE after adding the resource (before load adjustment)."""
        ...
    @property
    def resource_capacity_mw(self) -> float:
        """Resource nameplate capacity (MW)."""
        ...
    def to_dataframe(self) -> "pd.DataFrame":
        """Convert to a single-row DataFrame (columns: elcc_mw, elcc_fraction, lole_before, lole_after_addition, resource_capacity_mw)."""
        ...
    def __repr__(self) -> str: ...


class MonteCarloLoleResult:
    """Monte Carlo LOLE simulation result."""

    @property
    def lole_hours(self) -> float:
        """Expected LOLE in hours/period."""
        ...
    @property
    def lole_days(self) -> float:
        """Expected LOLE in days/period (unique days with LOL events)."""
        ...
    @property
    def eue_mwh(self) -> float:
        """Expected unserved energy (MWh/period)."""
        ...
    @property
    def lole_std_error(self) -> float:
        """Standard error of LOLE estimate."""
        ...
    @property
    def lole_ci_95(self) -> tuple[float, float]:
        """95% confidence interval for LOLE (hours)."""
        ...
    @property
    def n_trials(self) -> int:
        """Number of Monte Carlo trials."""
        ...
    @property
    def cv(self) -> float:
        """Coefficient of variation."""
        ...
    def __repr__(self) -> str: ...


class RenewableElccResult:
    """Renewable ELCC (Effective Load Carrying Capability) result."""

    @property
    def elcc_mw(self) -> float:
        """ELCC in MW."""
        ...
    @property
    def elcc_fraction(self) -> float:
        """ELCC as fraction of nameplate [0, 1]."""
        ...
    @property
    def lole_before(self) -> float:
        """LOLE before adding the renewable resource."""
        ...
    @property
    def lole_target(self) -> float:
        """Target LOLE for ELCC calculation."""
        ...
    @property
    def resource_capacity_mw(self) -> float:
        """Resource nameplate capacity (MW)."""
        ...
    @property
    def resource_name(self) -> str:
        """Resource name."""
        ...
    def __repr__(self) -> str: ...
    @property
    def augmented_lole_hours(self) -> Any: ...
    @property
    def base_lole_hours(self) -> Any: ...
    @property
    def capacity_credit(self) -> float: ...
    @property
    def installed_capacity_mw(self) -> float: ...


class MultiAreaLoleResult:
    """Multi-area LOLE result."""

    @property
    def area_lole_hours(self) -> list[float]:
        """LOLE in hours per area."""
        ...
    @property
    def area_lole_days(self) -> list[float]:
        """LOLE in days per area."""
        ...
    @property
    def area_eue_mwh(self) -> list[float]:
        """Expected unserved energy per area (MWh)."""
        ...
    @property
    def system_lole_hours(self) -> float:
        """System-wide LOLE in hours."""
        ...
    @property
    def n_areas(self) -> int:
        """Number of areas."""
        ...
    def __repr__(self) -> str: ...
    @property
    def area_names(self) -> list: ...
    @property
    def system_eue_mwh(self) -> Any: ...


class SequentialMcResult:
    """Sequential Monte Carlo simulation result."""

    @property
    def lole_hours(self) -> float:
        """Expected LOLE in hours/year."""
        ...
    @property
    def lole_days(self) -> float:
        """Expected LOLE in days/year."""
        ...
    @property
    def eue_mwh(self) -> float:
        """Expected unserved energy (MWh/year)."""
        ...
    @property
    def n_years(self) -> int:
        """Number of simulated years."""
        ...
    def __repr__(self) -> str: ...
    @property
    def eue_mwh_per_year(self) -> float: ...
    @property
    def lolh_hours_per_year(self) -> float: ...
    @property
    def lolp(self) -> float: ...


class ContingencyOptions:
    """Options for contingency analysis (analyze_n1_branch, analyze_n2_branch, etc.).

    All parameters have sensible defaults::

        opts = ContingencyOptions()                      # all defaults
        opts = ContingencyOptions(screening="lodf")      # LODF pre-screening
        opts = ContingencyOptions(thermal_threshold_pct=90.0, vm_min=0.90)
    """

    screening: str
    thermal_threshold_pct: float
    thermal_rating: str | None
    vm_min: float
    vm_max: float
    lodf_screening_pct: float
    top_k: int | None
    corrective_dispatch: bool
    detect_islands: bool
    voltage_stress_mode: str
    l_index_threshold: float
    store_post_voltages: bool
    contingency_flat_start: bool
    discrete_controls: bool
    include_breaker_contingencies: bool

    def __init__(
        self,
        screening: str = "fdpf",
        thermal_threshold_pct: float = 100.0,
        thermal_rating: str | None = None,
        vm_min: float = 0.95,
        vm_max: float = 1.05,
        lodf_screening_pct: float = 80.0,
        top_k: int | None = None,
        corrective_dispatch: bool = False,
        detect_islands: bool = True,
        voltage_stress_mode: str = "proxy",
        l_index_threshold: float = 0.7,
        store_post_voltages: bool = False,
        contingency_flat_start: bool = False,
        discrete_controls: bool = False,
        include_breaker_contingencies: bool = False,
    ) -> None: ...


class ContingencyAnalysis:
    """N-1 (or N-2) contingency analysis result."""

    @property
    def n_contingencies(self) -> int:
        """Total contingencies analyzed."""
        ...
    @property
    def n_screened_out(self) -> int:
        """Contingencies screened out by LODF/FDPF filter.

        Branch contingencies may be screened out by LODF or FDPF. Generator,
        breaker, HVDC, and other non-branch contingencies bypass branch-only
        LODF screening and proceed to AC validation.
        """
        ...
    @property
    def n_ac_solved(self) -> int:
        """Contingencies solved with full AC Newton-Raphson."""
        ...
    @property
    def n_converged(self) -> int:
        """AC-solved contingencies that converged."""
        ...
    @property
    def n_with_violations(self) -> int:
        """Contingencies with at least one thermal or voltage violation."""
        ...
    @property
    def solve_time_secs(self) -> float:
        """Wall-clock analysis time (seconds)."""
        ...
    @property
    def n_violations(self) -> int:
        """Total number of violations found across all contingencies."""
        ...
    @property
    def n_voltage_critical(self) -> int:
        """Number of contingencies classified as Critical or Unstable for voltage stability."""
        ...
    def to_dataframe(self) -> pd.DataFrame:
        """Return dict for pd.DataFrame. Columns: contingency_id, label, n_violations, converged, max_overload_pct, max_l_index, vsm_category."""
        ...
    def voltage_critical_df(self) -> pd.DataFrame:
        """DataFrame of Critical/Unstable contingencies sorted by L-index descending.

        Columns: contingency_id, label, max_l_index, critical_bus, vsm_category.
        """
        ...
    def results_dataframe(self) -> pd.DataFrame:
        """Per-contingency summary as a dict for pd.DataFrame.

        Columns: contingency_id, label, converged, n_violations, max_loading_pct, min_vm_pu, n_islands.
        """
        ...
    def violations_dataframe(self) -> pd.DataFrame:
        """Flat violation table as a dict for pd.DataFrame.

        Columns: contingency_id, violation_type, from_bus, to_bus, bus_number,
        loading_pct, flow_mw, flow_mva, limit_mva, vm_pu, vm_limit_pu.
        """
        ...
    def validate(self) -> bool:
        """Validate all contingency results. Returns True if valid, raises ValueError if not."""
        ...
    def post_contingency_vm(
        self, contingency_id: str
    ) -> NDArray[np.float64] | None:
        """Post-contingency bus voltage magnitudes (p.u.) for a given contingency.

        Returns None if store_post_voltages was not enabled or contingency not found.
        """
        ...
    def post_contingency_va(
        self, contingency_id: str
    ) -> NDArray[np.float64] | None:
        """Post-contingency bus voltage angles (radians) for a given contingency."""
        ...
    def post_contingency_flows(
        self, contingency_id: str
    ) -> NDArray[np.float64] | None:
        """Post-contingency branch apparent power flows (MVA) for a given contingency."""
        ...
    def __repr__(self) -> str: ...


class Contingency:
    """A user-defined contingency (outage event) for use with analyze_contingencies().

    Unlike analyze_n1_branch() which runs all N-1 branches automatically, this class
    lets you specify exactly which elements to trip (branches and/or generators),
    enabling N-k or mixed contingencies.

    Simultaneous network modifications (PSS/E ``.con`` SET/CHANGE commands) can
    be attached via the *modifications* parameter.  Each modification is a dict
    with a ``"type"`` key matching a ``ContingencyModification`` variant, e.g.::

        {"type": "BranchTap", "from_bus": 1, "to_bus": 2, "circuit": "1", "tap": 1.05}
        {"type": "LoadSet",   "bus": 5, "p_mw": 100.0, "q_mvar": 30.0}

    Modifications are applied to the per-contingency network clone before the
    post-contingency power flow is solved; the base-case network is never mutated.
    """

    def __init__(
        self,
        id: str,
        branches: Optional[list[tuple[int, int, int | str]]] = None,
        generators: Optional[list[tuple[int, str]]] = None,
        three_winding_transformers: Optional[list[tuple[int, int, int, str]]] = None,
        label: Optional[str] = None,
        modifications: Optional[list[dict[str, Any]]] = None,
        switches: Optional[list[str]] = None,
    ) -> None:
        """Create a contingency definition.

        Args:
            id: Unique identifier string.
            branches: List of (from_bus, to_bus, circuit) tuples to trip.
            generators: List of (bus, machine_id) tuples to trip.
            three_winding_transformers: List of (bus_i, bus_j, bus_k, circuit)
                tuples for three-winding transformer trips. At evaluation time,
                the star bus is located and all three winding branches are tripped.
            label: Human-readable label (defaults to id if omitted).
            modifications: List of ``ContingencyModification`` dicts describing
                simultaneous network state changes (e.g. tap adjustments, load
                redispatch) applied at the same instant as the element outages.
                Each dict must have a ``"type"`` key.
            switches: List of switch/breaker mRIDs to open. When non-empty,
                the contingency engine opens these switches and rebuild_topologys
                the network before solving.
        """
        ...
    @property
    def id(self) -> str: ...
    @property
    def label(self) -> str: ...
    @property
    def branches(self) -> list[tuple[int, int, str]]: ...
    @property
    def generators(self) -> list[tuple[int, str]]: ...
    @property
    def three_winding_transformers(self) -> list[tuple[int, int, int, str]]: ...
    @property
    def switches(self) -> list[str]:
        """Switch/breaker mRIDs to open for breaker contingencies."""
        ...
    @property
    def modifications(self) -> list[dict[str, Any]]:
        """Simultaneous network modifications attached to this contingency.

        Each entry is a dict with a ``"type"`` key, e.g.
        ``{"type": "BranchTap", "from_bus": 1, "to_bus": 2, "circuit": 1, "tap": 1.05}``.
        Returns an empty list when no modifications are set.
        """
        ...
    def __repr__(self) -> str: ...


class CorrectiveAction:
    """A corrective action for use in Remedial Action Schemes (RAS/SPS).

    Use the static factory methods to create specific action types.
    """

    @staticmethod
    def gen_redispatch(bus: int, machine_id: str, delta_mw: float) -> CorrectiveAction:
        """Generator redispatch: adjust output by delta_mw at (bus, machine_id)."""
        ...
    @staticmethod
    def tap_change(
        from_bus: int, to_bus: int, circuit: int | str, new_tap: float
    ) -> CorrectiveAction:
        """Transformer tap change: set new tap ratio on branch (from, to, ckt)."""
        ...
    @staticmethod
    def shunt_switch(bus: int, delta_mvar: float) -> CorrectiveAction:
        """Shunt switching: change reactive injection by delta_mvar at bus."""
        ...
    @staticmethod
    def load_shed(bus: int, fraction: float) -> CorrectiveAction:
        """Load shedding: shed a fraction (0–1) of load at bus."""
        ...
    @property
    def action_type(self) -> str: ...
    @action_type.setter
    def action_type(self, value: str) -> None: ...
    @property
    def bus(self) -> int | None: ...
    @bus.setter
    def bus(self, value: int | None) -> None: ...
    @property
    def from_bus(self) -> int | None: ...
    @from_bus.setter
    def from_bus(self, value: int | None) -> None: ...
    @property
    def to_bus(self) -> int | None: ...
    @to_bus.setter
    def to_bus(self, value: int | None) -> None: ...
    @property
    def circuit(self) -> str | None: ...
    @circuit.setter
    def circuit(self, value: str | None) -> None: ...
    @property
    def machine_id(self) -> str | None: ...
    @machine_id.setter
    def machine_id(self, value: str | None) -> None: ...
    @property
    def delta_mw(self) -> float | None: ...
    @delta_mw.setter
    def delta_mw(self, value: float | None) -> None: ...
    @property
    def new_tap(self) -> float | None: ...
    @new_tap.setter
    def new_tap(self, value: float | None) -> None: ...
    @property
    def delta_mvar(self) -> float | None: ...
    @delta_mvar.setter
    def delta_mvar(self, value: float | None) -> None: ...
    @property
    def shed_fraction(self) -> float | None: ...
    @shed_fraction.setter
    def shed_fraction(self, value: float | None) -> None: ...
    def __repr__(self) -> str: ...


class RemedialAction:
    """A Remedial Action Scheme (RAS/SPS) definition.

    Schemes are applied in priority order (lower value fires first) with a
    power flow re-solve after each scheme fires.  Trigger conditions are
    re-evaluated after each re-solve so that lower-priority schemes may be
    skipped if higher-priority schemes already cleared violations.

    Pre-contingency arming conditions (evaluated against the base-case solved
    state) can gate scheme eligibility.  Schemes sharing an ``exclusion_group``
    are mutually exclusive — only the highest-priority triggered scheme fires.
    """

    def __init__(
        self,
        name: str,
        trigger_branches: list[tuple[int, int, str]] = ...,
        actions: list[CorrectiveAction] = ...,
        modifications: list[dict] | None = None,
        max_redispatch_mw: float = 1e9,
        priority: int = 0,
        exclusion_group: str | None = None,
        trigger_conditions: list[dict] = ...,
        arm_conditions: list[dict] = ...,
    ) -> None:
        """Create a RAS definition.

        Args:
            name: Unique name for this RAS.
            trigger_branches: List of (from_bus, to_bus, circuit) branch outages
                that activate this RAS.  Sugar for ``BranchOutaged`` trigger
                conditions — merged with ``trigger_conditions``.
            actions: Corrective actions to apply when triggered.
            modifications: Optional network modifications to apply when triggered
                (e.g. remedial switching). Each dict must have a ``"type"`` key
                matching a ``ContingencyModification`` variant (``"BranchClose"``,
                ``"BranchTap"``, ``"LoadSet"``, ``"ShuntAdjust"``, etc.).
            max_redispatch_mw: Maximum total redispatch allowed (MW).
            priority: Firing priority (lower = fires first).  Ties broken by
                definition order. Default: 0.
            exclusion_group: Mutual exclusion group name.  Within a group, only
                the highest-priority triggered scheme fires.
            trigger_conditions: Explicit post-contingency trigger conditions as
                dicts.  Merged with ``trigger_branches`` (OR semantics).
                Supported types::

                    {"type": "BranchOutaged", "branch_idx": 5}
                    {"type": "PostCtgBranchLoading", "branch_idx": 5, "threshold_pct": 110.0}
                    {"type": "PostCtgVoltageLow", "bus_number": 42, "threshold_pu": 0.92}
                    {"type": "PostCtgVoltageHigh", "bus_number": 42, "threshold_pu": 1.06}
                    {"type": "PostCtgFlowgateOverload", "flowgate_name": "WN", "threshold_pct": 100.0}
                    {"type": "PostCtgInterfaceOverload", "interface_name": "WN", "threshold_pct": 100.0}

            arm_conditions: Pre-contingency arming conditions as dicts.  ALL
                must be satisfied for the scheme to be armed (implicit AND).
                Supported types::

                    {"type": "BranchLoading", "branch_idx": 5, "threshold_pct": 80.0}
                    {"type": "VoltageLow", "bus_idx": 42, "threshold_pu": 0.95}
                    {"type": "VoltageHigh", "bus_idx": 42, "threshold_pu": 1.05}
                    {"type": "InterfaceFlow", "name": "WN", "branches": [(5, 1.0), (6, -1.0)], "threshold_mw": 5000.0}
                    {"type": "SystemGenerationAbove", "threshold_mw": 50000.0}
                    {"type": "SystemGenerationBelow", "threshold_mw": 20000.0}
        """
        ...
    @property
    def name(self) -> str: ...
    @name.setter
    def name(self, value: str) -> None: ...
    @property
    def trigger_branches(self) -> list[tuple[int, int, str]]: ...
    @trigger_branches.setter
    def trigger_branches(self, value: list[tuple[int, int, str]]) -> None: ...
    @property
    def trigger_conditions(self) -> list[dict[str, Any]]: ...
    @trigger_conditions.setter
    def trigger_conditions(self, value: list[dict[str, Any]]) -> None: ...
    @property
    def arm_conditions(self) -> list[dict[str, Any]]: ...
    @arm_conditions.setter
    def arm_conditions(self, value: list[dict[str, Any]]) -> None: ...
    @property
    def actions(self) -> list[CorrectiveAction]: ...
    @actions.setter
    def actions(self, value: list[CorrectiveAction]) -> None: ...
    @property
    def modifications(self) -> list[dict[str, Any]]: ...
    @modifications.setter
    def modifications(self, value: list[dict[str, Any]]) -> None: ...
    @property
    def max_redispatch_mw(self) -> float: ...
    @max_redispatch_mw.setter
    def max_redispatch_mw(self, value: float) -> None: ...
    @property
    def priority(self) -> int: ...
    @priority.setter
    def priority(self, value: int) -> None: ...
    @property
    def exclusion_group(self) -> str | None: ...
    @exclusion_group.setter
    def exclusion_group(self, value: str | None) -> None: ...
    def __repr__(self) -> str: ...


class AcAtcResult:
    """AC-aware Available Transfer Capability result."""

    @property
    def atc_mw(self) -> float:
        """Binding ATC in MW (minimum of thermal and voltage limits)."""
        ...
    @property
    def thermal_limit_mw(self) -> float:
        """Thermal headroom from DC PTDF (MW)."""
        ...
    @property
    def voltage_limit_mw(self) -> float:
        """Voltage-constrained headroom from FDPF sensitivity (MW)."""
        ...
    @property
    def limiting_bus(self) -> Optional[int]:
        """Index of the bus that limits voltage (None if thermal is binding)."""
        ...
    @property
    def binding_branch(self) -> Optional[int]:
        """Index of the branch that limits thermal transfer (None if voltage is binding)."""
        ...
    @property
    def limiting_constraint(self) -> str:
        """Which constraint is binding: 'thermal' or 'voltage'."""
        ...
    def __repr__(self) -> str: ...


class MultiTransferResult:
    @property
    def transfer_mw(self) -> list[float]:
        """Optimal transfer for each path in MW."""
        ...
    @property
    def binding_branch(self) -> list[int | None]:
        """Binding branch index for each path, if identifiable."""
        ...
    @property
    def total_weighted_transfer(self) -> float:
        """Objective value: weighted sum of the path transfers."""
        ...
    def __repr__(self) -> str: ...


class ExpansionSolution:
    """Least-cost capacity expansion solution."""

    @property
    def total_annual_cost(self) -> float:
        """Total annual cost ($/year)."""
        ...
    @property
    def investment_cost(self) -> float:
        """Investment cost ($/year)."""
        ...
    @property
    def operating_cost(self) -> float:
        """Operating cost ($/year)."""
        ...
    @property
    def total_new_capacity_mw(self) -> float:
        """Total new capacity built (MW)."""
        ...
    @property
    def peak_load_mw(self) -> float:
        """Peak load (MW)."""
        ...
    @property
    def reserve_margin(self) -> float:
        """Achieved reserve margin (fraction)."""
        ...
    @property
    def investments(self) -> list[tuple[str, float, float]]:
        """Investment decisions: list of (candidate_id, capacity_mw, annual_cost)."""
        ...
    @property
    def solve_time_secs(self) -> float: ...
    @property
    def converged(self) -> bool:
        """True if the expansion solver converged."""
        ...
    def to_dataframe(self) -> "pd.DataFrame":
        """Convert investment decisions to a DataFrame (columns: candidate_id, capacity_mw, annual_cost)."""
        ...
    def __repr__(self) -> str: ...


class ModalResult:
    """Electromechanical mode screening result (GENCLS classical model)."""

    @property
    def n_modes(self) -> int:
        """Number of oscillatory modes identified."""
        ...
    @property
    def n_electromechanical_modes(self) -> int:
        """Number of electromechanical modes (0.1–3.0 Hz band)."""
        ...
    @property
    def min_damping_ratio(self) -> float:
        """Minimum damping ratio across all modes (negative = unstable)."""
        ...
    @property
    def stability(self) -> str:
        """Overall stability: 'Stable', 'Marginal', or 'Unstable'."""
        ...
    @property
    def modes(self) -> list[tuple[float, float, bool]]:
        """List of (frequency_hz, damping_ratio, is_electromechanical)."""
        ...
    def to_dataframe(self) -> pd.DataFrame:
        """Return dict for pd.DataFrame. Columns: mode_id, freq_hz, damping_ratio, is_electromechanical, dominant_generator."""
        ...
    def __repr__(self) -> str: ...


# ---------------------------------------------------------------------------
# Voltage stability classes
# ---------------------------------------------------------------------------

class VoltageStressBus:
    """Per-bus base-case voltage-stress summary."""

    @property
    def bus_number(self) -> int:
        """External bus number."""
        ...
    @property
    def local_qv_stress_proxy(self) -> Optional[float]:
        """Cheap local Q-V proxy for PQ buses."""
        ...
    @property
    def exact_l_index(self) -> Optional[float]:
        """Exact Kessel-Glavitsch L-index for PQ buses."""
        ...
    @property
    def voltage_margin_to_vmin(self) -> float:
        """Voltage magnitude margin to ``Vmin`` in per-unit."""
        ...
    def __repr__(self) -> str: ...


class VoltageStressOptions:
    """Options for base-case voltage-stress evaluation."""

    @property
    def mode(self) -> str:
        """Voltage-stress mode: ``'off'``, ``'proxy'``, or ``'exact_l_index'``."""
        ...
    @property
    def l_index_threshold(self) -> float:
        """Threshold for ``'exact_l_index'`` category classification."""
        ...
    @property
    def tolerance(self) -> float:
        """AC power-flow mismatch tolerance."""
        ...
    @property
    def max_iterations(self) -> int:
        """Maximum AC power-flow iterations."""
        ...
    @property
    def flat_start(self) -> bool:
        """Whether to flat-start the AC power flow."""
        ...
    @property
    def dc_warm_start(self) -> bool:
        """Whether to initialize the AC solve from a DC warm start."""
        ...
    @property
    def enforce_q_limits(self) -> bool:
        """Whether to enforce generator reactive limits during the AC solve."""
        ...
    @property
    def vm_min(self) -> float:
        """Minimum allowed bus voltage during the AC solve."""
        ...
    @property
    def vm_max(self) -> float:
        """Maximum allowed bus voltage during the AC solve."""
        ...
    def __repr__(self) -> str: ...


class VoltageStressResult:
    """Base-case voltage-stress result with exact and proxy metrics."""

    @property
    def per_bus(self) -> list[VoltageStressBus]:
        """Per-bus voltage-stress entries."""
        ...
    @property
    def max_qv_stress_proxy(self) -> Optional[float]:
        """Maximum local Q-V stress proxy across PQ buses."""
        ...
    @property
    def critical_proxy_bus(self) -> Optional[int]:
        """Bus with the highest local Q-V stress proxy."""
        ...
    @property
    def max_l_index(self) -> Optional[float]:
        """Maximum exact L-index across PQ buses."""
        ...
    @property
    def critical_l_index_bus(self) -> Optional[int]:
        """Bus with the highest exact L-index."""
        ...
    @property
    def category(self) -> Optional[str]:
        """Voltage-stability category: ``'secure'``, ``'marginal'``, ``'critical'``, or ``'unstable'``."""
        ...
    def to_dataframe(self) -> "pd.DataFrame":
        """Convert per-bus metrics to a DataFrame (columns: bus_id, local_qv_stress_proxy, exact_l_index, voltage_margin_to_vmin)."""
        ...
    def __repr__(self) -> str: ...


class PtdfResult:
    """PTDF result for a monitored branch set."""

    @property
    def ptdf(self) -> NDArray[np.float64]:
        """PTDF matrix as a numpy array of shape ``(n_monitored, n_buses)``."""
        ...
    @property
    def bus_indices(self) -> list[int]:
        """Internal bus indices (column order)."""
        ...
    @property
    def bus_numbers(self) -> list[int]:
        """External bus numbers (column order)."""
        ...
    def get_row(self, branch_idx: int) -> NDArray[np.float64]:
        """PTDF row for one monitored branch (length ``n_buses``)."""
        ...
    @property
    def monitored_branches(self) -> list[int]:
        """Internal monitored branch indices (row order)."""
        ...
    @property
    def branch_from(self) -> list[int]:
        """External from-bus numbers for monitored branches (row order)."""
        ...
    @property
    def branch_to(self) -> list[int]:
        """External to-bus numbers for monitored branches (row order)."""
        ...
    def to_dataframe(self) -> "pd.DataFrame":
        """Return a DataFrame with (from_bus, to_bus) row index and bus numbers as columns."""
        ...
    def __repr__(self) -> str: ...


class LodfResult:
    """Rectangular LODF result for explicit monitored and outage branch sets."""

    @property
    def lodf(self) -> NDArray[np.float64]:
        """LODF matrix as numpy array of shape (n_monitored, n_outages)."""
        ...
    @property
    def monitored_branches(self) -> list[int]:
        """Internal monitored branch indices (row order)."""
        ...
    @property
    def outage_branches(self) -> list[int]:
        """Internal outage branch indices (column order)."""
        ...
    @property
    def monitored_from(self) -> list[int]:
        """External from-bus numbers for monitored branches (row order)."""
        ...
    @property
    def monitored_to(self) -> list[int]:
        """External to-bus numbers for monitored branches (row order)."""
        ...
    @property
    def outage_from(self) -> list[int]:
        """External from-bus numbers for outage branches (column order)."""
        ...
    @property
    def outage_to(self) -> list[int]:
        """External to-bus numbers for outage branches (column order)."""
        ...
    def to_dataframe(self) -> "pd.DataFrame":
        """Return a DataFrame with (from_bus, to_bus) row index and (outage_from, outage_to) column index."""
        ...
    def __repr__(self) -> str: ...


class LodfMatrixResult:
    """Dense all-pairs LODF matrix result."""

    @property
    def lodf(self) -> NDArray[np.float64]:
        """LODF matrix as numpy array of shape (n_branches, n_branches)."""
        ...
    @property
    def branch_from(self) -> list[int]:
        """External from-bus numbers (row/column order)."""
        ...
    @property
    def branch_to(self) -> list[int]:
        """External to-bus numbers (row/column order)."""
        ...
    def to_dataframe(self) -> "pd.DataFrame":
        """Return a DataFrame with (from_bus, to_bus) as both row and column index."""
        ...
    def __repr__(self) -> str: ...


class N2LodfResult:
    """N-2 LODF result with monitored/outage-pair metadata."""

    @property
    def lodf(self) -> NDArray[np.float64]:
        """N-2 LODF vector as a numpy array of shape (n_monitored,)."""
        ...
    @property
    def monitored_branches(self) -> list[int]:
        """Internal monitored branch indices (value order)."""
        ...
    @property
    def monitored_keys(self) -> list[tuple[int, int, str]]:
        """Stable monitored branch keys (value order)."""
        ...
    @property
    def outage_pair(self) -> tuple[int, int]:
        """Ordered outage branch indices."""
        ...
    @property
    def outage_pair_key(self) -> tuple[tuple[int, int, str], tuple[int, int, str]]:
        """Stable branch keys for the ordered outage pair."""
        ...
    def __repr__(self) -> str: ...


class N2LodfBatchResult:
    """Batched N-2 LODF result with monitored/outage-pair metadata."""

    @property
    def lodf(self) -> NDArray[np.float64]:
        """N-2 LODF matrix as a numpy array of shape (n_monitored, n_pairs)."""
        ...
    @property
    def monitored_branches(self) -> list[int]:
        """Internal monitored branch indices (row order)."""
        ...
    @property
    def monitored_keys(self) -> list[tuple[int, int, str]]:
        """Stable monitored branch keys (row order)."""
        ...
    @property
    def outage_pairs(self) -> list[tuple[int, int]]:
        """Ordered outage branch pairs (column order)."""
        ...
    @property
    def outage_pair_keys(self) -> list[tuple[tuple[int, int, str], tuple[int, int, str]]]:
        """Stable branch keys for ordered outage pairs (column order)."""
        ...
    def __repr__(self) -> str: ...


class PreparedDcStudy:
    """Reusable DC power flow and sensitivity study object."""

    def solve_pf(
        self,
        headroom_slack: bool = False,
        headroom_slack_buses: list[int] | None = None,
    ) -> DcPfResult: ...
    def compute_ptdf(
        self,
        monitored_branches: list[int] | None = None,
        bus_indices: list[int] | None = None,
        slack_weights: dict[int, float] | None = None,
        headroom_slack: bool = False,
        headroom_slack_buses: list[int] | None = None,
    ) -> PtdfResult: ...
    def compute_lodf(
        self,
        monitored_branches: list[int] | None = None,
        outage_branches: list[int] | None = None,
    ) -> LodfResult: ...
    def compute_lodf_matrix(
        self,
        branches: list[int] | None = None,
    ) -> LodfMatrixResult: ...
    def compute_otdf(
        self,
        monitored_branches: list[int],
        outage_branches: list[int],
        bus_indices: list[int] | None = None,
        slack_weights: dict[int, float] | None = None,
        headroom_slack: bool = False,
        headroom_slack_buses: list[int] | None = None,
    ) -> OtdfResult: ...
    def compute_n2_lodf(
        self,
        outage_pair: tuple[int, int],
        monitored_branches: list[int] | None = None,
    ) -> N2LodfResult: ...
    def compute_n2_lodf_batch(
        self,
        outage_pairs: list[tuple[int, int]],
        monitored_branches: list[int] | None = None,
    ) -> N2LodfBatchResult: ...
    def __repr__(self) -> str: ...


class OtdfResult:
    """OTDF result for a set of (monitored, outage) branch pairs.

    Returned by ``compute_otdf(network, monitored_branches, outage_branches)``.
    The canonical tensor shape is ``(n_monitored, n_outage, n_buses)``.
    Bridge-line outages produce vectors of ``float('inf')``.
    """

    @property
    def otdf(self) -> NDArray[np.float64]:
        """OTDF tensor with shape ``(n_monitored, n_outage, n_buses)``."""
        ...
    def get(self, monitored: int, outage: int) -> NDArray[np.float64]:
        """OTDF vector for a (monitored_branch_idx, outage_branch_idx) pair (length n_buses).

        Raises KeyError if the pair was not in the computed set.
        """
        ...
    @property
    def monitored_branches(self) -> list[int]:
        """Internal monitored branch indices in row order."""
        ...
    @property
    def outage_branches(self) -> list[int]:
        """Internal outage branch indices in column order."""
        ...
    @property
    def n_buses(self) -> int:
        """Number of buses on the OTDF bus axis."""
        ...
    @property
    def bus_indices(self) -> list[int]:
        """Internal bus indices for the OTDF bus axis."""
        ...
    @property
    def bus_numbers(self) -> list[int]:
        """External bus numbers for the bus axis."""
        ...
    @property
    def monitored_from(self) -> list[int]:
        """External from-bus numbers for monitored branches."""
        ...
    @property
    def monitored_to(self) -> list[int]:
        """External to-bus numbers for monitored branches."""
        ...
    @property
    def outage_from(self) -> list[int]:
        """External from-bus numbers for outage branches."""
        ...
    @property
    def outage_to(self) -> list[int]:
        """External to-bus numbers for outage branches."""
        ...
    def __repr__(self) -> str: ...


class GsfResult:
    """Generation Shift Factor matrix result."""

    @property
    def gsf(self) -> NDArray[np.float64]: ...
    @property
    def gen_buses(self) -> list[int]: ...
    @property
    def branch_from(self) -> list[int]: ...
    @property
    def branch_to(self) -> list[int]: ...


class InjectionCapabilityResult:
    """Per-bus injection capability result."""

    @property
    def by_bus(self) -> list[tuple[int, float]]: ...
    @property
    def failed_contingencies(self) -> list[int]: ...
    def to_dataframe(self) -> pd.DataFrame: ...




class BldfResult:
    """Bus Load Distribution Factor matrix result.

    ``matrix[b, l]`` is the change in per-unit flow on branch ``l`` per
    1 p.u. load increase at bus ``b`` (slack absorbs the difference).
    """

    @property
    def matrix(self) -> NDArray[np.float64]:
        """BLDF matrix (n_buses × n_branches)."""
        ...
    @property
    def bus_numbers(self) -> list[int]:
        """External bus numbers (row order)."""
        ...
    @property
    def branch_from(self) -> list[int]:
        """External from-bus numbers (column order)."""
        ...
    @property
    def branch_to(self) -> list[int]:
        """External to-bus numbers (column order)."""
        ...
    @property
    def n_buses(self) -> int:
        """Number of buses (rows)."""
        ...
    @property
    def n_branches(self) -> int:
        """Number of branches (columns)."""
        ...
    def __repr__(self) -> str: ...




class AfcResult:
    """Available Flowgate Capability result for a single flowgate."""

    @property
    def flowgate_name(self) -> str:
        """Flowgate name."""
        ...
    @property
    def afc_mw(self) -> float:
        """Available Flowgate Capability in MW."""
        ...
    @property
    def binding_branch(self) -> int:
        """Index of the branch that limits the flowgate."""
        ...
    @property
    def binding_contingency(self) -> int | None:
        """Contingency branch index (None if N-0 is binding)."""
        ...
    def __repr__(self) -> str: ...


class YBusResult:
    """Sparse Y-bus admittance matrix in CSC format (complex128).

    Use ``to_scipy()`` to convert to ``scipy.sparse.csc_matrix``.
    """

    @property
    def indptr(self) -> NDArray[np.int64]:
        """CSC column pointers (length n+1)."""
        ...
    @property
    def indices(self) -> NDArray[np.int64]:
        """CSC row indices (length nnz)."""
        ...
    @property
    def data(self) -> NDArray[np.complex128]:
        """CSC complex admittance values (length nnz)."""
        ...
    @property
    def shape(self) -> tuple[int, int]:
        """Matrix shape (n_buses, n_buses)."""
        ...
    @property
    def nnz(self) -> int:
        """Number of non-zero entries."""
        ...
    @property
    def bus_numbers(self) -> list[int]:
        """External bus numbers (row/column ordering)."""
        ...
    def to_scipy(self) -> Any:
        """Convert to ``scipy.sparse.csc_matrix`` (complex128). Requires scipy."""
        ...
    def to_dense(self) -> NDArray[np.complex128]:
        """Dense numpy array (complex128). Only practical for small networks."""
        ...
    def __repr__(self) -> str: ...


class JacobianResult:
    """Sparse Jacobian matrix in CSC format (float64).

    The Jacobian J = [H N; M L] maps voltage corrections to power mismatches.
    Rows: [dP(pvpq), dQ(pq)]; Columns: [dtheta(pvpq), dVm(pq)].
    """

    @property
    def indptr(self) -> NDArray[np.int64]:
        """CSC column pointers."""
        ...
    @property
    def indices(self) -> NDArray[np.int64]:
        """CSC row indices."""
        ...
    @property
    def data(self) -> NDArray[np.float64]:
        """CSC values (float64)."""
        ...
    @property
    def shape(self) -> tuple[int, int]:
        """Matrix shape (n_pvpq + n_pq, n_pvpq + n_pq)."""
        ...
    @property
    def nnz(self) -> int:
        """Number of non-zero entries."""
        ...
    @property
    def pvpq_buses(self) -> list[int]:
        """External bus numbers for PV+PQ buses (theta variable ordering)."""
        ...
    @property
    def pq_buses(self) -> list[int]:
        """External bus numbers for PQ buses (Vm variable ordering)."""
        ...
    def to_scipy(self) -> Any:
        """Convert to ``scipy.sparse.csc_matrix`` (float64). Requires scipy."""
        ...
    def to_dense(self) -> NDArray[np.float64]:
        """Dense numpy array (float64). Only practical for small networks."""
        ...
    def __repr__(self) -> str: ...


class _LsfResult:
    """Internal package helper for loss sensitivity factor results.

    Returned by ``_losses_compute_factors()``.
    """

    @property
    def bus_numbers(self) -> list[int]:
        """External bus numbers."""
        ...
    @property
    def lsf(self) -> NDArray[np.float64]:
        """Loss sensitivity factors per bus as 1-D numpy array."""
        ...
    @property
    def base_losses_mw(self) -> float:
        """Total system losses at base case (MW)."""
        ...
    def to_dataframe(self) -> pd.DataFrame:
        """Convert to DataFrame with columns ``bus_id`` and ``lsf``."""
        ...
    def __repr__(self) -> str: ...


# ---------------------------------------------------------------------------
# Probabilistic classes
# ---------------------------------------------------------------------------
# ---------------------------------------------------------------------------
# SCR / WSCR (Short Circuit Ratio)
# ---------------------------------------------------------------------------

class ScrResult:
    """Single-bus Short Circuit Ratio (SCR) result.

    SCR = Ssc / P_rated, where Ssc is the short-circuit MVA at the bus
    from the positive-sequence Thevenin impedance.
    """

    @property
    def bus(self) -> int:
        """External bus number."""
        ...
    @property
    def scr(self) -> float:
        """Short Circuit Ratio = Ssc / P_rated."""
        ...
    @property
    def ssc_mva(self) -> float:
        """Short-circuit MVA at this bus."""
        ...
    @property
    def x_over_r(self) -> float:
        """X/R ratio at the bus (|X_th| / |R_th|)."""
        ...
    @property
    def z_thevenin_pu(self) -> tuple[float, float]:
        """Thevenin impedance (real, imag) in per-unit."""
        ...
    def __repr__(self) -> str: ...


class WscrResult:
    """Multi-bus Weighted Short Circuit Ratio (WSCR) result.

    WSCR = sum(Ssc_i * P_i) / (sum(P_i))^2 following the ERCOT methodology.
    """

    @property
    def wscr(self) -> float:
        """Weighted Short Circuit Ratio for the cluster."""
        ...
    @property
    def buses(self) -> list[int]:
        """External bus numbers in the cluster."""
        ...
    @property
    def per_bus_ssc_mva(self) -> list[float]:
        """Short-circuit MVA at each bus."""
        ...
    @property
    def per_bus_mw_rating(self) -> list[float]:
        """MW rating of the IBR at each bus (from the input)."""
        ...
    def __repr__(self) -> str: ...


def compute_scr(
    network: Network,
    bus: int,
    gen_mw_rating: float,
) -> ScrResult:
    """Compute Short Circuit Ratio (SCR) at a single bus.

    SCR = Ssc / P_rated, where Ssc is the short-circuit MVA computed
    from the positive-sequence Thevenin impedance.

    Args:
        network: Power system network.
        bus: External bus number of the point of interconnection.
        gen_mw_rating: MW rating of the IBR (must be > 0).

    Returns:
        ScrResult with scr, ssc_mva, bus, and x_over_r.
    """
    ...


def compute_wscr(
    network: Network,
) -> WscrResult:
    """Compute Weighted Short Circuit Ratio (WSCR) for a cluster of IBR buses.

    Follows the ERCOT WSCR methodology:
      WSCR = sum(Ssc_i * P_i) / (sum(P_i))^2

    Args:
        network: Power system network.
        buses: List of (external_bus_number, mw_rating) tuples.

    Returns:
        WscrResult with wscr, buses, per_bus_ssc_mva, and per_bus_mw_rating.
    """
    ...


# ---------------------------------------------------------------------------
# Harmonics
# ---------------------------------------------------------------------------

class HarmonicResult:
    """Detailed result from a harmonic power flow analysis (solve_harmonic_pf)."""

    @property
    def harmonic_orders(self) -> list[int]: ...
    @property
    def ieee519_compliant(self) -> bool: ...
    @property
    def violations(self) -> list[tuple[int, float, float]]:
        """List of (bus_number, thd_pct, limit_pct) violation tuples."""
        ...
    @property
    def max_thd_pct(self) -> float: ...
    @property
    def max_thd_bus(self) -> int: ...
    def thd_voltage(self) -> NDArray[np.float64]:
        """Voltage THD (%) at each bus as 1-D numpy array."""
        ...
    def to_dataframe(self) -> pd.DataFrame:
        """Return dict for pd.DataFrame. Columns: bus_id, thd_voltage_pct, ieee519_ok, vh_h<N>_pu."""
        ...
    def __repr__(self) -> str: ...


class Iec61000Report:
    """IEC 61000-3-6 compliance report (check_iec61000)."""

    @property
    def voltage_level(self) -> str:
        """Voltage classification: 'Lv', 'Mv', 'Hv', 'Ehv'."""
        ...
    @property
    def thd_pct(self) -> float: ...
    @property
    def thd_limit_pct(self) -> float: ...
    @property
    def thd_compliant(self) -> bool: ...
    @property
    def overall_compliant(self) -> bool: ...
    @property
    def per_order(self) -> list[tuple[int, float, float, bool]]:
        """Per-order: list of (order, ihd_pct, limit_pct, compliant)."""
        ...
    def __repr__(self) -> str: ...


class HarmonicContingencyResult:
    """N-1 harmonic contingency analysis result (analyze_n1_harmonic)."""

    def per_bus(self) -> list[dict]:
        """Per-bus worst contingency: list of dicts with keys bus_number, base_thd_pct, worst_thd_pct, thd_increase_pct, worst_branch."""
        ...
    @property
    def branch_sensitivity(self) -> list[tuple[tuple[int, int, int], float]]:
        """Branch sensitivity ranking: ((from, to, ckt), max_thd_increase)."""
        ...
    @property
    def new_violations(self) -> list[tuple[tuple[int, int, int], int]]:
        """New violations: ((from, to, ckt), bus_number)."""
        ...
    def __repr__(self) -> str: ...


class RationalModel:
    """Rational transfer function model from Vector Fitting (FDNE)."""

    def evaluate_at_frequency(self, f_hz: float) -> tuple[float, float]:
        """Evaluate Z(f) and return (real, imag)."""
        ...
    def is_passive(self, f_min: float, f_max: float) -> bool:
        """Check passivity Re(Z) >= 0 over frequency range."""
        ...
    @property
    def n_poles(self) -> int: ...
    @property
    def d(self) -> float:
        """Constant term."""
        ...
    @property
    def e(self) -> float:
        """Proportional term coefficient."""
        ...
    def __repr__(self) -> str: ...


class BdewReport:
    """BDEW 2008 MV harmonic compliance report (check_bdew_2008)."""

    @property
    def compliant(self) -> bool:
        """True if all harmonics within BDEW MV limits."""
        ...
    @property
    def per_order(self) -> list[tuple[int, float, float, bool]]:
        """Per-order: list of (order, ihd_pct, limit_pct, compliant)."""
        ...
    def __repr__(self) -> str: ...


class Harmonic3phResult:
    """Three-phase unbalanced harmonic power flow result (solve_harmonic_3ph)."""

    @property
    def thd_per_phase(self) -> list[list[float]]:
        """Per-bus per-phase THD (%): list of [thd_a, thd_b, thd_c]."""
        ...
    @property
    def vuf(self) -> list[tuple[int, int, float]]:
        """Voltage unbalance factors: list of (bus_idx, order, vuf_pct)."""
        ...
    @property
    def n_buses(self) -> int: ...
    def per_bus(self) -> list[dict]:
        """Per-bus results with keys: bus_number, v1, harmonics."""
        ...
    def to_dataframe(self) -> pd.DataFrame:
        """DataFrame with columns: bus_id, thd_a_pct, thd_b_pct, thd_c_pct."""
        ...
    def __repr__(self) -> str: ...


# ---------------------------------------------------------------------------
# GIC
# ---------------------------------------------------------------------------

class GicResult:
    """GIC analysis result (compute_gic — network-based)."""

    def __repr__(self) -> str: ...
    @property
    def high_gic_transformer_buses(self) -> list: ...
    @property
    def line_currents_a(self) -> list: ...
    @property
    def max_gic_amps(self) -> Any: ...
    @property
    def nerc_threshold_exceeded(self) -> bool: ...
    @property
    def total_q_demand_mvar(self) -> float: ...
    @property
    def transformer_gic_amps(self) -> list: ...


class GicStudyResult:
    """Full GIC analysis result (compute_gic_parametric — parametric)."""

    @property
    def total_q_absorbed_mvar(self) -> float: ...
    @property
    def high_gic_transformer_buses(self) -> list[int]: ...
    @property
    def max_gic_amps(self) -> float: ...
    @property
    def nerc_tpl007_risk(self) -> str:
        """NERC TPL-007 risk: 'Low', 'Medium', 'High', or 'Extreme'."""
        ...
    def gic_amps(self) -> NDArray[np.float64]:
        """Effective GIC per phase at each transformer (A) as 1-D numpy array."""
        ...
    def reactive_absorbed(self) -> NDArray[np.float64]:
        """Reactive power absorbed by each transformer (MVAr) as 1-D numpy array."""
        ...
    def to_dataframe(self) -> pd.DataFrame:
        """Return dict for pd.DataFrame. Columns: substation_bus, dc_voltage_v."""
        ...
    def to_transformer_dataframe(self) -> pd.DataFrame:
        """Return dict for pd.DataFrame. Columns: transformer_index, gic_amps, reactive_mvar, high_gic."""
        ...
    def __repr__(self) -> str: ...


# ---------------------------------------------------------------------------
# SSR
# ---------------------------------------------------------------------------

class SsrResult:
    """Subsynchronous resonance analysis result."""

    @property
    def resonance_hz(self) -> float:
        """Electrical series resonance frequency (Hz)."""
        ...
    @property
    def complement_freq_hz(self) -> float:
        """Complement (subsynchronous) frequency = f0 - resonance_hz (Hz)."""
        ...
    @property
    def at_risk(self) -> bool:
        """True if any torsional mode has net negative damping."""
        ...
    @property
    def n_at_risk_modes(self) -> int: ...
    @property
    def max_risk_mode(self) -> Optional[int]:
        """Index of the highest-risk mode (smallest margin), or None."""
        ...
    def torsional_modes_hz(self) -> NDArray[np.float64]:
        """Torsional mode natural frequencies (Hz) as 1-D numpy array."""
        ...
    def damping_ratios(self) -> NDArray[np.float64]:
        """Mechanical damping ratios per mode as 1-D numpy array."""
        ...
    def electrical_damping(self) -> NDArray[np.float64]:
        """Electrical damping at complement frequency per mode as 1-D numpy array."""
        ...
    def z_scan(self) -> list[tuple[float, float]]:
        """Frequency-scan data as list of (freq_hz, |Z_drive|) tuples."""
        ...
    def to_dataframe(self) -> pd.DataFrame:
        """Return dict for pd.DataFrame. Columns: mode_id, freq_hz, complement_freq_hz, damping_ratio, electrical_damping, at_risk."""
        ...
    def __repr__(self) -> str: ...


# ---------------------------------------------------------------------------
# Arc flash
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Motor
# ---------------------------------------------------------------------------

class InductionMotor:
    """Three-phase induction motor equivalent circuit parameters."""

    @property
    def name(self) -> str: ...
    @property
    def rated_kw(self) -> float: ...
    @property
    def rated_voltage_kv(self) -> float: ...
    @property
    def poles(self) -> int: ...
    @property
    def frequency_hz(self) -> float: ...
    @property
    def synchronous_speed_rpm(self) -> float: ...
    @property
    def z_base_ohm(self) -> float: ...
    @property
    def full_load_current_a(self) -> float: ...
    @property
    def locked_rotor_current_a(self) -> float: ...
    @property
    def full_load_efficiency(self) -> float:
        """Full-load efficiency (fraction 0–1)."""
        ...

    def __init__(
        self,
        rated_kw: float,
        voltage_kv: float,
        poles: int = 4,
        frequency_hz: float = 60.0,
    ) -> None: ...
    @staticmethod
    def nema_b_small() -> InductionMotor:
        """Return a NEMA Design B small motor (≈15 kW, 400 V, 4-pole, 60 Hz)."""
        ...
    @staticmethod
    def nema_b_medium() -> InductionMotor:
        """Return a NEMA Design B medium motor (≈75 kW, 400 V, 4-pole, 60 Hz)."""
        ...
    @staticmethod
    def nema_b_large() -> InductionMotor:
        """Return a NEMA Design B large motor (≈750 kW, 4.16 kV, 4-pole, 60 Hz)."""
        ...
    def torque_speed_curve(self) -> tuple[list[float], list[float]]:
        """Compute torque-speed curve. Returns (speeds_rpm, torques_nm)."""
        ...
    def __repr__(self) -> str: ...


class MotorStartResult:
    """Result from a motor start simulation (analyze_motor_start)."""

    def to_dataframe(self) -> pd.DataFrame: ...
    def __repr__(self) -> str: ...
    @property
    def min_voltage_pu(self) -> float: ...
    @property
    def peak_current_pu(self) -> float: ...
    @property
    def start_success(self) -> bool: ...
    @property
    def start_time_s(self) -> float: ...
    @property
    def torque_at_start_nm(self) -> float: ...


class MotorOperatingPoint:
    """Operating point result for an induction motor (compute_motor_operating_point)."""

    def __repr__(self) -> str: ...


# ---------------------------------------------------------------------------
# Resiliency
# ---------------------------------------------------------------------------
    @property
    def current_a(self) -> Any: ...
    @property
    def efficiency_pct(self) -> float: ...
    @property
    def power_factor(self) -> float: ...
    @property
    def power_in_kw(self) -> Any: ...
    @property
    def power_mech_kw(self) -> Any: ...
    @property
    def slip(self) -> float: ...
    @property
    def speed_rpm(self) -> float: ...
    @property
    def torque_nm(self) -> float: ...
    @property
    def voltage_kv(self) -> float: ...

class ResiliencyResult:
    """Grid resiliency analysis result (IEEE 1366 reliability indices)."""

    def __repr__(self) -> str: ...


# ---------------------------------------------------------------------------
# Line parameters
# ---------------------------------------------------------------------------
    @property
    def asai(self) -> float: ...
    @property
    def caidi_hours(self) -> Any: ...
    @property
    def criticality_scores(self) -> list: ...
    @property
    def load_impact_mw(self) -> float: ...
    @property
    def n_critical_elements(self) -> int: ...
    @property
    def saidi_hours_per_year(self) -> float: ...
    @property
    def saifi_per_year(self) -> float: ...
    @property
    def top_critical_branches(self) -> list: ...

class LineParametersResult:
    """Computed transmission line parameters (Carson's equations)."""

    """Positive-sequence resistance (Ohm/km)."""
    """Positive-sequence reactance (Ohm/km)."""
    """Positive-sequence susceptance (uS/km)."""
    """Zero-sequence resistance (Ohm/km)."""
    """Zero-sequence reactance (Ohm/km)."""
    """Zero-sequence susceptance (uS/km)."""
    """Total positive-sequence impedance magnitude |Z1| (Ohm) = hypot(R1, X1) * length_km."""
    """Total zero-sequence impedance magnitude |Z0| (Ohm) = hypot(R0, X0) * length_km."""
    """Line length (km)."""
    def __repr__(self) -> str: ...


# ---------------------------------------------------------------------------
# HVDC
# ---------------------------------------------------------------------------
    @property
    def b0_us_per_km(self) -> float: ...
    @property
    def b1_us_per_km(self) -> float: ...
    @property
    def length_km(self) -> Any: ...
    @property
    def r0_ohm_per_km(self) -> float: ...
    @property
    def r1_ohm_per_km(self) -> float: ...
    @property
    def x0_ohm_per_km(self) -> float: ...
    @property
    def x1_ohm_per_km(self) -> float: ...
    @property
    def z0_total_ohm(self) -> float: ...
    @property
    def z1_total_ohm(self) -> float: ...

    @property
    def power_factor(self) -> float:
        """Commutation power factor cos(φ)."""
        ...


# ---------------------------------------------------------------------------
# Three-phase unbalanced power flow
# ---------------------------------------------------------------------------

class ThreePhaseNetwork:
    """A three-phase power network (buses + branches)."""

    def __init__(self, name: str = "unnamed", base_mva: float = 100.0) -> None: ...
    def n_buses(self) -> int: ...
    def __repr__(self) -> str: ...


class ThreePhaseSolution:
    """Result of a three-phase unbalanced power flow solve."""

    @property
    def converged(self) -> bool: ...
    @property
    def iterations(self) -> int: ...
    @property
    def max_mismatch(self) -> float: ...
    @property
    def vuf_limit_violated(self) -> bool:
        """True if any bus exceeds VUF > 2% (IEC industrial limit)."""
        ...
    @property
    def max_imbalance_pct(self) -> float:
        """Maximum VUF across all buses (%)."""
        ...
    def va_mag(self) -> NDArray[np.float64]:
        """Phase-A voltage magnitudes at each bus (p.u.) as 1-D numpy array."""
        ...
    def vb_mag(self) -> NDArray[np.float64]:
        """Phase-B voltage magnitudes at each bus (p.u.) as 1-D numpy array."""
        ...
    def vc_mag(self) -> NDArray[np.float64]:
        """Phase-C voltage magnitudes at each bus (p.u.) as 1-D numpy array."""
        ...
    def unbalance_factor(self) -> NDArray[np.float64]:
        """Voltage Unbalance Factor (VUF) per bus in % as 1-D numpy array."""
        ...
    def __repr__(self) -> str: ...


# ---------------------------------------------------------------------------
# Protection
# ---------------------------------------------------------------------------

class CoordCheckResult:
    """Two-relay coordination check result."""

    """Coordination Time Interval (upstream time minus downstream time) in seconds."""
    def __repr__(self) -> str: ...
    @property
    def coordinated(self) -> bool: ...
    @property
    def cti_seconds(self) -> float: ...
    @property
    def violation(self) -> str: ...


class OvercurrentRelay:
    """Overcurrent relay with IEC/IEEE inverse-time characteristics."""

    def __init__(
        self,
        id: str,
        branch_id: str,
        pickup_current: float,
        tds: float = 1.0,
        curve: str = "iec_extremely_inverse",
        ct_ratio: float = 1.0,
        instantaneous_pickup: float | None = None,
        direction: str = "forward",
    ) -> None: ...
    @property
    def id(self) -> str: ...
    @property
    def branch_id(self) -> str: ...
    @property
    def pickup_current(self) -> float: ...
    @property
    def tds(self) -> float: ...
    @property
    def ct_ratio(self) -> float: ...
    @property
    def instantaneous_pickup(self) -> float | None: ...
    @property
    def curve(self) -> str: ...
    @property
    def direction(self) -> str: ...


# ---------------------------------------------------------------------------
# Distribution
# ---------------------------------------------------------------------------

class DistNetwork:
    """A radial distribution network for BFS power flow."""

    def n_buses(self) -> int: ...
    def n_branches(self) -> int: ...
    def __repr__(self) -> str: ...


class DistSolution:
    """Solution from a distribution BFS power flow."""

    @property
    def converged(self) -> bool: ...
    @property
    def iterations(self) -> int: ...
    @property
    def total_losses_kw(self) -> float: ...
    @property
    def total_losses_kvar(self) -> float: ...
    @property
    def overloaded_branches(self) -> list[int]: ...
    @property
    def voltage_violations(self) -> list[int]: ...
    def bus_vm(self) -> NDArray[np.float64]:
        """Bus voltage magnitudes (p.u.) as 1-D numpy array."""
        ...
    def bus_va_deg(self) -> NDArray[np.float64]:
        """Bus voltage angles (degrees) as 1-D numpy array."""
        ...
    def branch_flow_kva(self) -> NDArray[np.float64]:
        """Branch apparent power flow magnitudes (kVA) as 1-D numpy array."""
        ...
    def __repr__(self) -> str: ...


class ThreePhaseDistNetwork:
    """A three-phase distribution network for unbalanced BFS power flow."""

    @property
    def n_buses(self) -> int:
        """Number of buses."""
        ...
    @property
    def n_branches(self) -> int:
        """Number of branches (lines)."""
        ...
    @property
    def n_transformers(self) -> int:
        """Number of transformers."""
        ...
    @property
    def n_loads(self) -> int:
        """Number of loads."""
        ...
    @property
    def source_bus(self) -> int:
        """Source bus index (feeder head)."""
        ...
    @property
    def source_voltage_kv(self) -> float:
        """Nominal line-to-line voltage at source (kV)."""
        ...
    def bus_names(self) -> list[str]:
        """Bus names as list of strings."""
        ...
    def __repr__(self) -> str: ...


class ThreePhaseBfsResult:
    """Three-phase unbalanced BFS power flow result."""

    @property
    def converged(self) -> bool:
        """True if BFS solver converged."""
        ...
    @property
    def iterations(self) -> int:
        """Number of BFS iterations."""
        ...
    @property
    def total_loss_kw(self) -> float:
        """Total three-phase real losses (kW)."""
        ...
    @property
    def total_loss_kvar(self) -> float:
        """Total three-phase reactive losses (kVAr)."""
        ...
    @property
    def max_vuf_pct(self) -> float:
        """Maximum voltage unbalance factor across all buses (%)."""
        ...
    def vuf_pct(self) -> NDArray[np.float64]:
        """VUF per bus in percent (1-D array)."""
        ...
    def v_nom_ln_kv(self) -> NDArray[np.float64]:
        """Per-bus nominal phase-to-neutral voltage (kV)."""
        ...
    def va_kv(self) -> NDArray[np.float64]:
        """Phase-A voltage magnitudes (kV L-N)."""
        ...
    def vb_kv(self) -> NDArray[np.float64]:
        """Phase-B voltage magnitudes (kV L-N)."""
        ...
    def vc_kv(self) -> NDArray[np.float64]:
        """Phase-C voltage magnitudes (kV L-N)."""
        ...
    def va_ang_deg(self) -> NDArray[np.float64]:
        """Phase-A voltage angles (degrees)."""
        ...
    def vb_ang_deg(self) -> NDArray[np.float64]:
        """Phase-B voltage angles (degrees)."""
        ...
    def vc_ang_deg(self) -> NDArray[np.float64]:
        """Phase-C voltage angles (degrees)."""
        ...
    def __repr__(self) -> str: ...


class StochasticHcResult:
    """Per-bus stochastic Monte Carlo hosting capacity result."""

    @property
    def n_trials(self) -> int: ...
    def hc_mean(self) -> NDArray[np.float64]:
        """Mean hosting capacity per bus (kW) as 1-D numpy array."""
        ...
    def hc_p10(self) -> NDArray[np.float64]:
        """10th-percentile hosting capacity per bus (kW) as 1-D numpy array."""
        ...
    def hc_p50(self) -> NDArray[np.float64]:
        """50th-percentile (median) hosting capacity per bus (kW) as 1-D numpy array."""
        ...
    def hc_p90(self) -> NDArray[np.float64]:
        """90th-percentile hosting capacity per bus (kW) as 1-D numpy array."""
        ...
    def __repr__(self) -> str: ...


# ---------------------------------------------------------------------------
# Wave 4/5 classes
# ---------------------------------------------------------------------------

class InterharmonicResult:
    """Single spectral component (interharmonic/subharmonic) injection result."""

    """'Harmonic', 'Interharmonic', or 'Subharmonic'."""
    def voltage_magnitudes(self) -> NDArray[np.float64]:
        """Voltage magnitudes (p.u.) at each bus from this injection as 1-D numpy array."""
        ...
    def __repr__(self) -> str: ...
    @property
    def frequency_hz(self) -> float: ...
    @property
    def order(self) -> int: ...
    @property
    def source_type(self) -> str: ...


class BusVoltageRisk:
    """Voltage risk statistics for a single bus (from compute_voltage_risk)."""

    def __repr__(self) -> str: ...
    @property
    def bus_idx(self) -> int: ...
    @property
    def mean_voltage_pu(self) -> float: ...
    @property
    def p05_voltage_pu(self) -> float: ...
    @property
    def p95_voltage_pu(self) -> float: ...
    @property
    def p_overvoltage(self) -> float: ...
    @property
    def p_undervoltage(self) -> float: ...
    @property
    def std_voltage_pu(self) -> float: ...


class VoltageRiskResult:
    """System-wide voltage risk metrics from Monte Carlo voltage samples."""

    @property
    def bus_stats(self) -> list[BusVoltageRisk]: ...
    def to_dataframe(self) -> pd.DataFrame:
        """Return dict for pd.DataFrame. Columns: bus_idx, p_undervoltage, p_overvoltage, mean_voltage_pu, std_voltage_pu, p05_voltage_pu, p95_voltage_pu."""
        ...
    def __repr__(self) -> str: ...
    @property
    def expected_violations(self) -> float: ...
    @property
    def n_buses(self) -> int: ...
    @property
    def n_samples(self) -> int: ...
    @property
    def p_system_violation(self) -> float: ...


# ---------------------------------------------------------------------------
# Core module functions
# ---------------------------------------------------------------------------

def init_logging(level: str = "warn", json: bool = False) -> None:
    """Initialize Rust-side logging. Call once before any solver functions.

    Args:
        level: "error", "warn", "info", "debug", or "trace".
               RUST_LOG env var overrides this.
        json: If True, emit machine-readable JSON logs.
    """
    ...


def set_max_threads(n: int) -> None:
    """Set the maximum number of threads for parallel computation.

    Configures rayon's global thread pool. Must be called before any parallel
    function (e.g. ``analyze_n1_branch``, ``parameter_sweep``). Once initialized the
    pool cannot be resized; subsequent calls have no effect.

    Args:
        n: Maximum number of worker threads (>= 1).
    """
    ...


def get_max_threads() -> int:
    """Return the number of threads in the global thread pool.

    If ``set_max_threads`` was never called, returns the number of logical CPUs.
    """
    ...


def version() -> str:
    """Return the Surge library version string."""
    ...


def load(path: str) -> Network:
    """Load a power system case file.

    Auto-detects format from file extension. If *path* is a directory,
    it is treated as a CGMES multi-file bundle (all ``*.xml`` inside).

    Supported formats:
        - ``.m`` — MATPOWER
        - ``.raw`` — PSS/E RAW (v30–v36)
        - ``.rawx`` — PSS/E RAWX (JSON-based)
        - ``.cdf`` — IEEE Common Data Format
        - ``.xiidm`` / ``.iidm`` — PowSyBl XIIDM
        - ``.uct`` / ``.ucte`` — UCTE-DEF
        - ``.xml`` / ``.cim`` — CGMES (single-file)
        - ``.epc`` — GE PSLF
        - ``.dss`` — OpenDSS
        - ``.json`` — Surge JSON
        - ``.zip`` — CGMES zip bundle
        - directory — CGMES multi-file bundle

    Args:
        path: Path to the case file or CGMES directory.

    Returns:
        Network object.
    """
    ...


def save(network: Network, path: str) -> None:
    """Save a network to a file, auto-detecting format from extension.

    Supported extensions:
        - ``.m`` — MATPOWER
        - ``.raw`` — PSS/E RAW
        - ``.epc`` — GE PSLF EPC
        - ``.xiidm`` / ``.iidm`` — PowSyBl XIIDM
        - ``.uct`` / ``.ucte`` — UCTE-DEF
        - ``.dss`` — OpenDSS
        - ``.json`` — Surge JSON

    CGMES export is explicit and directory-based through ``surge.io.cgmes.save(...)``.

    Args:
        network: Network to save.
        path: Destination file path.
    """
    ...


def _units_ohm_to_pu(ohm: float, base_kv: float, base_mva: float = 100.0) -> float:
    """Internal package helper for unit conversion."""
    ...


def _load_as(path: str, format: str) -> Network:
    """Internal package helper for explicit-format file loads."""
    ...


def _loads(content: str, format: str) -> Network:
    """Internal package helper for explicit-format string loads."""
    ...


def _loads_bytes(content: bytes, format: str) -> Network:
    """Internal package helper for explicit-format byte loads."""
    ...


def _save_as(
    network: Network,
    path: str,
    format: str,
    version: int | None = None,
) -> None:
    """Internal package helper for explicit-format file saves."""
    ...


def _dumps(
    network: Network,
    format: str,
    version: int | None = None,
) -> str:
    """Internal package helper for explicit-format string dumps."""
    ...


def _dumps_bytes(network: Network, format: str) -> bytes:
    """Internal package helper for explicit-format byte dumps."""
    ...


def _io_json_save(network: Network, path: str, pretty: bool = False) -> None:
    """Internal package helper for Surge JSON file saves."""
    ...


def _io_json_dumps(network: Network, pretty: bool = False) -> str:
    """Internal package helper for Surge JSON string dumps."""
    ...


class _CgmesProfiles:
    """Internal package helper for in-memory CGMES profile strings."""

    eq: str
    tp: str
    ssh: str
    sv: str
    sc: str | None
    me: str | None
    asset: str | None
    ol: str | None
    bd: str | None
    pr: str | None
    no: str | None


def _io_cgmes_save(
    network: Network,
    output_dir: str,
    version: str = "2.4.15",
) -> None:
    """Internal package helper for explicit CGMES directory saves."""
    ...


def _io_cgmes_to_profiles(
    network: Network,
    version: str = "2.4.15",
) -> _CgmesProfiles:
    """Internal package helper for in-memory CGMES profile generation."""
    ...


def _io_export_write_network_csv(network: Network, output_dir: str) -> None:
    """Internal package helper for CSV network export."""
    ...


def _io_export_write_solution_snapshot(
    solution: AcPfResult,
    network: Network,
    output_path: str,
) -> None:
    """Internal package helper for flat solved-state CSV export."""
    ...


def _io_geo_apply_bus_coordinates(network: Network, csv_path: str) -> int:
    """Internal package helper for bus coordinate enrichment."""
    ...


class _SeqStats:
    """Internal package helper for PSS/E sequence-data apply statistics."""

    machines_updated: int
    branches_updated: int
    transformers_updated: int
    mutual_couplings: int
    skipped_records: int

    def __repr__(self) -> str: ...


def _io_psse_sequence_apply(network: Network, path: str) -> _SeqStats:
    """Internal package helper for applying sequence data from a file."""
    ...


def _io_psse_sequence_apply_text(network: Network, content: str) -> _SeqStats:
    """Internal package helper for applying sequence data from a string."""
    ...


def _losses_compute_factors(
    network: Network,
    solution: AcPfResult | None = None,
) -> _LsfResult:
    """Internal package helper for AC loss sensitivity factors."""
    ...


def analyze_contingencies(
    network: Network,
    contingencies: list[Contingency],
    options: ContingencyOptions | None = None,
    monitored_branches: list[tuple[int, int, str]] | None = None,
) -> ContingencyAnalysis:
    """Compute AC contingency analysis for a user-defined contingency list.

    Unlike analyze_n1_branch() which runs all N-1 branch contingencies, this function
    accepts an explicit list so you can analyze specific elements, N-k outages,
    or mixed branch+generator events.

    Args:
        network: Power system network.
        contingencies: List of Contingency objects.
        options: Contingency analysis options. If None, uses defaults.
            See :class:`ContingencyOptions` for all available parameters.
        monitored_branches: Optional list of (from_bus, to_bus, circuit) tuples to
            restrict which branches are monitored for thermal violations.

    Returns:
        ContingencyAnalysis with per-contingency results.
    """
    ...


def apply_ras(
    network: Network,
    ca_result: ContingencyAnalysis,
    ras: list[RemedialAction],
) -> ContingencyAnalysis:
    """Apply Remedial Action Schemes to contingency analysis results.

    Schemes are applied in priority order with a power flow re-solve after
    each scheme fires.  Trigger conditions are re-evaluated after each
    re-solve.  A base-case AC power flow is solved once for arming condition
    evaluation.

    Per-scheme audit outcomes are stored on each ``ContingencyResult`` in the
    ``scheme_outcomes`` field (list of dicts with ``scheme_name``, ``priority``,
    and ``status`` keys).

    Args:
        network: Power system network (base case).
        ca_result: Original contingency analysis results.
        ras: List of RemedialAction definitions.

    Returns:
        Updated ContingencyAnalysis with RAS-corrected results and scheme
        outcomes.
    """
    ...


def apply_opf_dispatch(network: Network, sol: OpfResult) -> None:
    """Apply OPF solution dispatch back into a Network (modifies in place).

    Sets each in-service generator's ``pg`` to the OPF-optimal value.

    Args:
        network: Network to update (modified in place).
        sol: OPF solution whose dispatch is applied.
    """
    ...


def apply_dispatch_mw(network: Network, gen_p_mw: list[float]) -> None:
    """Apply a generator dispatch vector (MW) from a SCED/SCUC period back into a Network.

    Args:
        network: Network to update (modified in place).
        gen_p_mw: Generator dispatch in MW, one per in-service generator.
    """
    ...


def apply_bus_voltages(
    network: Network,
    vm: list[float],
    va_rad: list[float],
) -> None:
    """Stamp bus voltage magnitudes and angles onto the network.

    Args:
        network: Network to update (modified in place).
        vm: Voltage magnitudes (p.u.), indexed in ``network.buses`` order.
        va_rad: Voltage angles (radians), indexed in ``network.buses`` order.
    """
    ...


def solve_dc_pf(
    network: Network,
    headroom_slack: bool = False,
    headroom_slack_buses: list[int] | None = None,
    participation_factors: dict[int, float] | None = None,
    angle_reference: str = "preserve_initial",
) -> DcPfResult:
    """Solve DC power flow.

    Args:
        network: Power system network.
        headroom_slack: When True, redistribute power imbalance across
            in-service generator buses according to available generator
            headroom instead of absorbing it at the single slack bus.
        headroom_slack_buses: Explicit list of participating bus numbers for
            headroom-limited slack balancing. Overrides ``headroom_slack``.
        participation_factors: Explicit bus-number-to-weight map for
            distributed slack participation.
        angle_reference: Output angle reference convention:
            ``"preserve_initial"``, ``"zero"``, ``"distributed"``,
            ``"distributed_load"``, ``"distributed_generation"``, or
            ``"distributed_inertia"``.

    Returns:
        DcPfResult with ``.va_rad`` / ``.va_deg``, ``.branch_p_mw``,
        ``.slack_p_mw``, ``.solve_time_secs``, ``.to_dataframe()``,
        and ``.branch_dataframe()``.
    """
    ...


def solve_ac_pf(
    network: Network,
    tolerance: float = 1e-8,
    max_iterations: int = 100,
    flat_start: bool = False,
    oltc: bool = True,
    switched_shunts: bool = True,
    oltc_max_iter: int = 20,
    distributed_slack: bool = True,
    slack_participation: dict[int, float] | None = None,
    enforce_interchange: bool = False,
    interchange_max_iter: int = 10,
    enforce_q_limits: bool = True,
    enforce_gen_p_limits: bool = True,
    merge_zero_impedance: bool = False,
    dc_warm_start: bool = True,
    startup_policy: str = "adaptive",
    q_sharing: str = "capability",
    warm_start: AcPfResult | None = None,
    line_search: bool = True,
    detect_islands: bool = True,
    dc_line_model: str = "fixed_schedule",
    record_convergence_history: bool = False,
    vm_min: float = 0.5,
    vm_max: float = 1.5,
    angle_reference: str = "preserve_initial",
) -> AcPfResult:
    """Solve AC power flow using Newton-Raphson (KLU sparse solver).

    If the network has OLTC controls (added via network.add_oltc_control()) or
    switched shunts (via network.add_switched_shunt()), an outer loop iterates
    until all controls are within their dead-bands or oltc_max_iter is reached.

    Args:
        network: Power system network.
        tolerance: Convergence tolerance in per-unit (must be finite and positive).
        max_iterations: Maximum inner NR iterations per outer step.
        flat_start: If True, start from flat voltage profile (1.0 pu, 0 degrees).
        oltc: Enable OLTC tap-stepping outer loop (default True).
        switched_shunts: Enable switched shunt outer loop (default True).
        oltc_max_iter: Maximum outer OLTC/shunt iterations (default 20).
        distributed_slack: When True (and slack_participation is None), distribute
            real-power mismatch equally among all in-service generator buses.
        slack_participation: Explicit bus-number → participation-factor mapping.
            Factors are normalised internally and need not sum to 1. Overrides
            ``distributed_slack``.
        enforce_interchange: Enforce area interchange targets via outer loop
            (default False).
        interchange_max_iter: Maximum outer-loop iterations for area interchange
            enforcement (default 10).
        enforce_q_limits: Enforce generator reactive power limits via PV→PQ
            switching (default True).
        enforce_gen_p_limits: When True (default), generators with pg < pmin
            are treated as inactive for voltage regulation.
        merge_zero_impedance: If True, merge zero-impedance buses before solving.
        dc_warm_start: When True (default) and flat_start=True, initialise
            voltage angles from a DC power flow solve.
        startup_policy: ``"adaptive"`` (default), ``"single"``, or ``"parallel_warm_and_flat"``.
        q_sharing: ``"capability"`` (default), ``"mbase"``, or ``"equal"``.
        warm_start: Prior AcPfResult to warm-start from. Overrides
            flat_start and dc_warm_start when provided.
        line_search: Enable backtracking line search to prevent divergence
            (default True).
        detect_islands: Detect and solve islands independently (default True).
            Set False for known-connected networks for a small performance gain.
        dc_line_model: ``"fixed_schedule"`` (default) injects DC line P/Q once;
            ``"sequential_ac_dc"`` iterates AC and DC operating points.
        record_convergence_history: If True, populate
            ``convergence_history`` on the result (default False).
        vm_min: Lower voltage magnitude clamp (default 0.5 p.u.).
        vm_max: Upper voltage magnitude clamp (default 1.5 p.u.).
        angle_reference: ``"preserve_initial"`` (default), ``"zero"``, or
            ``"distributed"``.

    Returns:
        AcPfResult with vm, va_rad, gen_q_mvar, iterations, converged, status.

    Raises:
        ConvergenceError: If the solver fails to converge.
        SurgeError: For other solver failures (bad network, NaN voltages, etc.).
    """
    ...


class HvdcLccDetail:
    alpha_deg: float
    gamma_deg: float
    i_dc_pu: float
    power_factor: float

    def __repr__(self) -> str: ...


class HvdcStationSolution:
    name: str | None
    technology: str
    ac_bus: int
    dc_bus: int | None
    p_ac_mw: float
    q_ac_mvar: float
    p_dc_mw: float
    v_dc_pu: float
    converter_loss_mw: float
    converged: bool

    @property
    def lcc_detail(self) -> HvdcLccDetail | None: ...
    @property
    def power_balance_error_mw(self) -> float: ...

    def __repr__(self) -> str: ...


class HvdcDcBusSolution:
    dc_bus: int
    voltage_pu: float

    def __repr__(self) -> str: ...


class HvdcResult:
    total_converter_loss_mw: float
    total_dc_network_loss_mw: float
    total_loss_mw: float
    iterations: int
    converged: bool
    method: str

    @property
    def stations(self) -> list[HvdcStationSolution]: ...
    @property
    def dc_buses(self) -> list[HvdcDcBusSolution]: ...

    def __repr__(self) -> str: ...


def solve_hvdc(
    network: Network,
    method: str = "auto",
    tol: float = 1e-6,
    max_iter: int = 50,
    ac_tol: float = 1e-8,
    max_ac_iter: int = 100,
    dc_tol: float = 1e-8,
    max_dc_iter: int = 50,
    flat_start: bool = True,
    coupling_sensitivities: bool = True,
    coordinated_droop: bool = True,
) -> HvdcResult:
    """Solve HVDC power flow using the canonical Rust HVDC API."""
    ...


class PreparedAcPf:
    """Cached AC power flow solver for repeated solves on the same network.

    Pre-computes Y-bus, KLU symbolic factorization, and workspace on
    construction. Subsequent solves reuse all cached structures.

    Ideal for contingency screening, time-series, and parameter sweeps.

    Note: outer loops (OLTC, switched shunts, PAR, Q-limit switching,
    island detection) are not supported. Use solve_ac_pf() for those.
    """

    def __init__(
        self,
        network: Network,
        tolerance: float = 1e-8,
        max_iterations: int = 100,
        line_search: bool = True,
        dc_warm_start: bool = True,
        record_convergence_history: bool = False,
    ) -> None: ...
    def solve(self) -> AcPfResult:
        """Solve from case-data initial conditions."""
        ...
    def solve_with_warm_start(self, prior: AcPfResult) -> AcPfResult:
        """Solve warm-started from a prior solution."""
        ...
    def solve_with_flat_start(self) -> AcPfResult:
        """Solve from flat start (Vm=1.0, Va=0.0)."""
        ...
    def __repr__(self) -> str: ...


def solve_fdpf(
    network: Network,
    tolerance: float = 1e-6,
    max_iterations: int = 100,
    flat_start: bool = True,
    variant: str = "xb",
    enforce_q_limits: bool = True,
) -> AcPfResult:
    """Solve AC power flow using Fast Decoupled Power Flow.

    Args:
        network: Power system network.
        tolerance: Convergence tolerance (p.u. mismatch).
        max_iterations: Maximum iterations.
        flat_start: If True (default), start from Vm=1.0, Va=0.0.
            If False, initialise from case data (PV/slack setpoints).
        variant: FDPF variant: ``"xb"`` (default) or ``"bx"``.
        enforce_q_limits: Enforce generator reactive power limits via
            PV→PQ switching (default True).

    Returns:
        AcPfResult with converged flag, iterations, vm, va_rad, solve_time_secs.
    """
    ...


def compute_ptdf(
    network: Network,
    monitored_branches: list[int] | None = None,
    bus_indices: list[int] | None = None,
    slack_weights: dict[int, float] | None = None,
    headroom_slack: bool = False,
    headroom_slack_buses: list[int] | None = None,
) -> PtdfResult:
    """Compute PTDF for a subset of monitored branches (memory-efficient).

    Uses one KLU sparse solve per monitored branch — does NOT materialize
    B'^-1. Suitable for any network size.

    Args:
        network: Power system network.
        monitored_branches: Optional list of internal branch indices to compute
            PTDF for. When omitted, computes PTDF for all branches.
        bus_indices: Optional list of internal bus indices for the PTDF bus axis.
            When omitted, returns PTDF for all buses.

    Returns:
        PtdfResult with ``.ptdf`` matrix and monitored-branch metadata.
    """
    ...


def prepare_dc_study(network: Network) -> PreparedDcStudy:
    """Prepare reusable DC power flow and sensitivity study state."""
    ...


def compute_lodf(
    network: Network,
    monitored_branches: list[int] | None = None,
    outage_branches: list[int] | None = None,
) -> LodfResult:
    """Compute LODF for explicit monitored and outage branch sets.

    Uses sparse KLU solves and returns a rectangular monitored-by-outage matrix.

    Args:
        network: Power system network.
        monitored_branches: Optional list of internal monitored branch indices.
            When omitted, monitors all branches.
        outage_branches: Optional list of internal outage branch indices.
            When omitted, uses the monitored branch set.

    Returns:
        LodfResult with ``.lodf`` as a numpy array of shape (n_monitored, n_outages).
    """
    ...


def compute_lodf_matrix(
    network: Network,
    branches: list[int] | None = None,
) -> LodfMatrixResult:
    """Compute dense all-pairs LODF matrix for the given branch set.

    Uses sparse KLU solves — does NOT materialize B'^-1.

    Args:
        network: Power system network.
        branches: Optional list of internal branch indices used as both the
            monitored set and outage set. When omitted, computes the full branch set.

    Returns:
        LodfMatrixResult with ``.lodf`` as a numpy array of shape (n_branches, n_branches).
    """
    ...

def compute_n2_lodf(
    network: Network,
    outage_pair: tuple[int, int],
    monitored_branches: list[int] | None = None,
) -> N2LodfResult:
    """Compute N-2 LODF factors for a simultaneous double outage.

    Args:
        network: Power system network.
        outage_pair: Two internal branch indices forming the simultaneous outage.
        monitored_branches: Optional list of internal branch indices to monitor.
            When omitted, computes factors for all branches.

    Returns:
        1-D numpy array with one N-2 LODF factor per monitored branch.
    """
    ...


def compute_n2_lodf_batch(
    network: Network,
    outage_pairs: list[tuple[int, int]],
    monitored_branches: list[int] | None = None,
) -> N2LodfBatchResult:
    """Compute N-2 LODF factors for a batch of simultaneous double outages.

    Args:
        network: Power system network.
        outage_pairs: Ordered list of internal branch-index outage pairs.
        monitored_branches: Optional list of internal branch indices to monitor.
            When omitted, computes factors for all branches.

    Returns:
        N2LodfBatchResult with a monitored-by-pair matrix and outage-pair metadata.
    """
    ...

def compute_otdf(
    network: Network,
    monitored_branches: list[int],
    outage_branches: list[int],
    bus_indices: list[int] | None = None,
    slack_weights: dict[int, float] | None = None,
    headroom_slack: bool = False,
    headroom_slack_buses: list[int] | None = None,
) -> OtdfResult:
    """Compute Outage Transfer Distribution Factors (OTDF).

    ``OTDF[(m, k)][bus] = PTDF[m][bus] + LODF[m, k] × PTDF[k][bus]``

    This is the post-contingency sensitivity of flow on monitored branch ``m``
    to a 1 p.u. injection at ``bus`` when outage branch ``k`` is tripped.

    Factors B' once; one KLU solve per unique branch in the union of the two index sets.
    Bridge-line outages produce OTDF vectors of ``float('inf')``.

    Args:
        network: Power system network.
        monitored_branches: List of internal branch indices to monitor.
        outage_branches: List of internal branch indices to outage.
        bus_indices: Optional list of internal bus indices for the OTDF bus axis.

    Returns:
        OtdfResult with ``.otdf`` shaped ``(n_monitored, n_outage, n_buses)``.
    """
    ...


def compute_gsf(network: Network) -> GsfResult:
    """Compute Generation Shift Factor matrix.

    Args:
        network: Power system network.

    Returns:
        GsfResult with ``.gsf`` shaped ``(n_branches, n_generators)``, ``.gen_buses``,
        ``.branch_from``, ``.branch_to``.
    """
    ...


def compute_injection_capability(
    network: Network,
    post_contingency_rating_fraction: float = 1.0,
    exact: bool = False,
    monitored_branches: list[int] | None = None,
    contingency_branches: list[int] | None = None,
    slack_weights: list[tuple[int, float]] | None = None,
) -> InjectionCapabilityResult:
    """Compute per-bus injection capability considering N-1 constraints.

    Args:
        network: Power system network.
        post_contingency_rating_fraction: Fraction of branch rating for post-contingency limit (default 1.0).
        exact: When true, re-solves each outage exactly instead of using first-order LODF screening.
        monitored_branches: Optional monitored branch index set.
        contingency_branches: Optional outage branch index set.

    Returns:
        InjectionCapabilityResult with ``.by_bus`` list and ``.to_dataframe()`` method.
    """
    ...


def compute_bldf(network: Network) -> BldfResult:
    """Compute Bus Load Distribution Factor matrix.

    BLDF[b, l] = change in per-unit flow on branch l per 1 p.u.
    load increase at bus b (slack absorbs the difference).

    Args:
        network: Power system network.

    Returns:
        BldfResult with matrix (n_buses × n_branches), bus_numbers,
        branch_from, branch_to.
    """
    ...


def compute_afc(
    network: Network,
    path: TransferPath,
    flowgates: list[Flowgate],
) -> list[AfcResult]:
    """Compute Available Flowgate Capability for a list of flowgates.

    Args:
        network: Power system network.
        path: Canonical transfer path.
        flowgates: Flowgate definitions to evaluate against the path.

    Returns:
        List of AfcResult, one per flowgate.
    """
    ...


def compute_ac_atc(
    network: Network,
    path: TransferPath,
    v_min: float = 0.95,
    v_max: float = 1.05,
) -> AcAtcResult:
    """Compute AC-aware Available Transfer Capability with reactive margin constraints.

    Args:
        network: Power system network.
        path: Canonical transfer path.
        v_min: Minimum allowable bus voltage in p.u.
        v_max: Maximum allowable bus voltage in p.u.

    Returns:
        AcAtcResult with atc_mw, thermal_limit_mw, voltage_limit_mw, limiting_bus, limiting_constraint.
    """
    ...


class NercAtcResult:
    """NERC Available Transfer Capability result (MOD-029/MOD-030)."""

    @property
    def atc_mw(self) -> float:
        """Available Transfer Capability in MW (TTC - TRM - CBM - ETC)."""
        ...
    @property
    def ttc_mw(self) -> float:
        """Total Transfer Capability in MW (raw thermal headroom)."""
        ...
    @property
    def trm_mw(self) -> float:
        """Transmission Reliability Margin applied in MW."""
        ...
    @property
    def cbm_mw(self) -> float:
        """Capacity Benefit Margin applied in MW."""
        ...
    @property
    def etc_mw(self) -> float:
        """Existing Transmission Commitments in MW."""
        ...
    @property
    def limit_cause(self) -> str:
        """Explicit limit-cause kind: unconstrained, basecase_thermal, contingency_thermal, or fail_closed_outage."""
        ...
    @property
    def binding_branch(self) -> int | None:
        """Index of the monitored branch that binds the transfer, if any."""
        ...
    @property
    def binding_contingency(self) -> int | None:
        """Index of the contingency branch that binds or forces fail-closed behavior, if any."""
        ...
    @property
    def monitored_branches(self) -> list[int]:
        """Indices of monitored branches, same order as transfer_ptdf."""
        ...
    @property
    def reactive_margin_warning(self) -> bool:
        """Whether a reactive margin warning was triggered."""
        ...
    @property
    def transfer_ptdf(self) -> list[float]:
        """Transfer PTDFs for each monitored branch."""
        ...
    def __repr__(self) -> str: ...


class TransferPath:
    """Directional transfer path used by ATC, AFC, and multi-transfer studies."""

    def __init__(self, name: str, source_buses: list[int], sink_buses: list[int]) -> None: ...
    @property
    def name(self) -> str: ...
    @name.setter
    def name(self, value: str) -> None: ...
    @property
    def source_buses(self) -> list[int]: ...
    @source_buses.setter
    def source_buses(self, value: list[int]) -> None: ...
    @property
    def sink_buses(self) -> list[int]: ...
    @sink_buses.setter
    def sink_buses(self, value: list[int]) -> None: ...
    def __repr__(self) -> str: ...


class Flowgate:
    """Flowgate definition for AFC studies."""

    def __init__(
        self,
        name: str,
        monitored_branch: int,
        normal_rating_mw: float,
        contingency_branch: int | None = None,
        contingency_rating_mw: float | None = None,
    ) -> None: ...
    @property
    def name(self) -> str: ...
    @name.setter
    def name(self, value: str) -> None: ...
    @property
    def monitored_branch(self) -> int: ...
    @monitored_branch.setter
    def monitored_branch(self, value: int) -> None: ...
    @property
    def contingency_branch(self) -> int | None: ...
    @contingency_branch.setter
    def contingency_branch(self, value: int | None) -> None: ...
    @property
    def normal_rating_mw(self) -> float: ...
    @normal_rating_mw.setter
    def normal_rating_mw(self, value: float) -> None: ...
    @property
    def contingency_rating_mw(self) -> float | None: ...
    @contingency_rating_mw.setter
    def contingency_rating_mw(self, value: float | None) -> None: ...
    def __repr__(self) -> str: ...


class AtcOptions:
    """Options controlling monitored branches, contingencies, and NERC margins."""

    def __init__(
        self,
        monitored_branches: list[int] | None = None,
        contingency_branches: list[int] | None = None,
        trm_fraction: float = 0.05,
        cbm_mw: float = 0.0,
        etc_mw: float = 0.0,
    ) -> None: ...
    @property
    def monitored_branches(self) -> list[int] | None: ...
    @monitored_branches.setter
    def monitored_branches(self, value: list[int] | None) -> None: ...
    @property
    def contingency_branches(self) -> list[int] | None: ...
    @contingency_branches.setter
    def contingency_branches(self, value: list[int] | None) -> None: ...
    @property
    def trm_fraction(self) -> float: ...
    @trm_fraction.setter
    def trm_fraction(self, value: float) -> None: ...
    @property
    def cbm_mw(self) -> float: ...
    @cbm_mw.setter
    def cbm_mw(self, value: float) -> None: ...
    @property
    def etc_mw(self) -> float: ...
    @etc_mw.setter
    def etc_mw(self, value: float) -> None: ...
    def __repr__(self) -> str: ...


class TransferStudy:
    """Prepared transfer-study model for repeated ATC, AFC, and multi-transfer runs."""

    def compute_nerc_atc(
        self,
        path: TransferPath,
        options: AtcOptions | None = None,
    ) -> NercAtcResult:
        """Compute NERC ATC using the prepared study state."""
        ...

    def compute_afc(
        self,
        path: TransferPath,
        flowgates: list[Flowgate],
    ) -> list[AfcResult]:
        """Compute AFC using the prepared study state."""
        ...

    def compute_ac_atc(
        self,
        path: TransferPath,
        v_min: float = 0.95,
        v_max: float = 1.05,
    ) -> AcAtcResult:
        """Compute AC-aware ATC using the prepared study state."""
        ...

    def compute_multi_transfer(
        self,
        paths: list[TransferPath],
        weights: list[float] | None = None,
        max_transfer_mw: list[float] | None = None,
    ) -> MultiTransferResult:
        """Compute simultaneous transfer across multiple paths using the prepared study state."""
        ...

    def compute_injection_capability(
        self,
        post_contingency_rating_fraction: float = 1.0,
        exact: bool = False,
        monitored_branches: list[int] | None = None,
        contingency_branches: list[int] | None = None,
        slack_weights: list[tuple[int, float]] | None = None,
    ) -> InjectionCapabilityResult:
        """Compute per-bus injection capability using prepared study state."""
        ...

    def __repr__(self) -> str: ...


def compute_nerc_atc(
    network: Network,
    path: TransferPath,
    options: AtcOptions | None = None,
) -> NercAtcResult:
    """Compute NERC Available Transfer Capability (MOD-029/MOD-030).

    ATC = TTC - TRM - CBM - ETC.

    Args:
        network: Power system network.
        path: Canonical transfer path.
        options: Optional monitored branches, contingencies, and NERC margins.

    Returns:
        NercAtcResult with atc_mw, ttc_mw, trm_mw, cbm_mw, etc_mw, limit_cause, binding_branch, binding_contingency.
    """
    ...


def prepare_transfer_study(network: Network) -> TransferStudy:
    """Prepare reusable transfer-study state for repeated ATC, AFC, and interface runs."""
    ...


def compute_multi_transfer(
    network: Network,
    paths: list[TransferPath],
    weights: list[float] | None = None,
    max_transfer_mw: list[float] | None = None,
) -> MultiTransferResult:
    """Compute simultaneous transfer capability across multiple paths.

    Args:
        network: Power system network.
        paths: Canonical transfer paths to optimize jointly.
        weights: Optional objective weight per interface. Defaults to all ones.
        max_transfer_mw: Optional MW upper bound per interface. Defaults to a large bound.

    Returns:
        MultiTransferResult with one transfer value and binding branch per path.
    """
    ...


def solve_dc_opf(
    network: Network,
    tolerance: float = 1e-8,
    enforce_thermal_limits: bool = True,
    lp_solver: Optional[str] = None,
    use_pwl_costs: bool = False,
    pwl_cost_breakpoints: int = 20,
    enforce_flowgates: bool = True,
    warm_start_theta: Optional[list[float]] = None,
    par_setpoints: Optional[list[dict]] = None,
    hvdc_links: Optional[list[dict]] = None,
    gen_limit_penalty: Optional[float] = None,
    virtual_bids: Optional[list[dict]] = None,
    max_iterations: int = 200,
    min_rate_a: float = 1.0,
    use_loss_factors: bool = False,
    max_loss_iter: int = 3,
    loss_tol: float = 1e-3,
) -> OpfResult:
    """Solve DC Optimal Power Flow (sparse B-theta formulation with HiGHS).

    Args:
        network: Power system network with generator cost curves.
        tolerance: Solver tolerance.
        enforce_thermal_limits: Whether to enforce branch thermal (MVA) limits.
        lp_solver: LP solver backend ('highs' or 'gurobi'). Default: HiGHS.
        use_pwl_costs: Use piecewise-linear epigraph for quadratic costs. Default False.
        pwl_cost_breakpoints: Number of tangent-line breakpoints per PWL generator.
        enforce_flowgates: Enforce interface and base-case flowgate limits. Default True.
        warm_start_theta: Initial bus angle vector (radians) for warm-starting.
        par_setpoints: List of dicts with keys: from_bus (int), to_bus (int),
            circuit (str), target_mw (float).
        hvdc_links: List of HVDC link dicts for co-optimization. Each dict has keys:
            from_bus (int), to_bus (int), p_max (float), p_min (float),
            loss_fraction (float). Default: None (no HVDC).
        gen_limit_penalty: When set, adds gen-limit slack variables with this
            penalty cost ($/MW) for feasibility analysis. Default: None.

    Returns:
        OpfResult with gen_p_mw, lmp, total_cost, flowgate_shadow_prices.
    """
    ...


def solve_dc_opf_full(
    network: Network,
    tolerance: float = 1e-8,
    enforce_thermal_limits: bool = True,
    lp_solver: Optional[str] = None,
    use_pwl_costs: bool = False,
    pwl_cost_breakpoints: int = 20,
    enforce_flowgates: bool = True,
    warm_start_theta: Optional[list[float]] = None,
    par_setpoints: Optional[list[dict]] = None,
    hvdc_links: Optional[list[dict]] = None,
    gen_limit_penalty: Optional[float] = None,
    virtual_bids: Optional[list[dict]] = None,
    max_iterations: int = 200,
    min_rate_a: float = 1.0,
    use_loss_factors: bool = False,
    max_loss_iter: int = 3,
    loss_tol: float = 1e-3,
) -> DcOpfResult: ...


def solve_ac_opf(
    network: Network,
    tolerance: float = 1e-8,
    max_iterations: int = 0,
    exact_hessian: bool = True,
    nlp_solver: Optional[str] = None,
    print_level: int = 0,
    enforce_thermal_limits: bool = True,
    thermal_limit_slack_penalty_per_mva: float = 0.0,
    min_rate_a: float = 1.0,
    enforce_angle_limits: bool = False,
    warm_start: Optional[OpfResult] = None,
    warm_start_vm_pu: Optional[list[float]] = None,
    warm_start_va_rad: Optional[list[float]] = None,
    use_dc_opf_warm_start: Optional[bool] = None,
    optimize_switched_shunts: bool = False,
    optimize_taps: bool = False,
    optimize_phase_shifters: bool = False,
    include_hvdc: Optional[bool] = None,
    enforce_capability_curves: bool = True,
    discrete_mode: str = "continuous",
    optimize_svc: bool = False,
    optimize_tcsc: bool = False,
    dt_hours: float = 1.0,
    enforce_flowgates: bool = False,
    constraint_screening_threshold: Optional[float] = None,
    constraint_screening_min_buses: int = 1000,
    screening_fallback_enabled: bool = False,
    storage_soc_override: Optional[dict[int, float]] = None,
) -> AcOpfHvdcResult:
    """Solve AC Optimal Power Flow with the selected NLP backend.

    Args:
        network: Power system network with generator cost curves.
        tolerance: NLP convergence tolerance (must be finite and positive).
        max_iterations: Maximum NLP iterations (0 = auto: max(500, n_buses/20)).
        exact_hessian: If True (default), use exact analytical Hessian.
            Set False to use L-BFGS (fewer memory requirements, more NLP iterations).
        nlp_solver: NLP solver backend. Default: best available runtime backend
            (currently COPT, then Ipopt, then Gurobi).
        print_level: NLP solver verbosity (0=silent, 5=verbose). Default: 0.
        enforce_thermal_limits: Enforce branch thermal flow limits. Default: True.
        min_rate_a: Minimum rate_a (MVA) for a branch to have a thermal limit enforced.
            Branches with rate_a below this are treated as unconstrained. Default: 1.0.
        enforce_angle_limits: Enforce branch angle-difference limits (angmin/angmax).
            Default: False — many case files encode the current operating angle rather
            than a binding operational limit; enable only when limits are genuine.
        warm_start: Prior OpfResult to use as NLP warm start (Vm, Va, Pg, Qg).
            Typically halves solver iterations for sequential market clearing.
        warm_start_vm_pu: Explicit initial bus voltage magnitudes in per-unit.
            Overrides the bus-voltage magnitudes from ``warm_start`` when both
            are provided.
        warm_start_va_rad: Explicit initial bus voltage angles in radians.
            Overrides the bus-voltage angles from ``warm_start`` when both are
            provided.
        use_dc_opf_warm_start: Seed initial angles from a DC-OPF solution.
            None (default) = auto-enable when n_buses > 2000 and no warm_start.
            True = force DC-OPF warm start; False = use simple DC power flow angles.
        optimize_switched_shunts: Co-optimize switched shunt banks as NLP variables.
        optimize_taps: Co-optimize transformer tap ratios as NLP variables.
        optimize_phase_shifters: Co-optimize phase-shifting transformer angles as NLP variables.
        include_hvdc: Include HVDC modeling in AC-OPF.
            None (default) = auto-detect from network data.
            Point-to-point HVDC links use sequential AC-DC iteration.
            Explicit DC topology is co-optimized in the joint AC-DC NLP path.
        optimize_svc: Co-optimize SVC/STATCOM susceptance as continuous NLP variables.
        optimize_tcsc: Co-optimize TCSC compensating reactance as continuous NLP variables.
        enforce_capability_curves: Enforce generator P-Q capability curve (D-curve)
            constraints. Default: True. When False, generators use flat rectangular
            Qmin/Qmax bounds (simpler NLP, faster convergence).
        discrete_mode: Discrete variable handling mode. Default: 'continuous'.
            'continuous': solve the continuous NLP relaxation (default).
            'round-and-check': after solving the continuous NLP, round transformer
            taps, phase shifters, and switched shunts to their nearest discrete
            step, then verify feasibility via AC power flow.

    Returns:
        AcOpfHvdcResult with the AC-OPF solution plus HVDC dispatch and loss
        vectors. When discrete_mode='round-and-check', the nested OpfResult
        also populates tap_dispatch, phase_dispatch, discrete_feasible, and
        discrete_violations.
    """
    ...


def solve_scopf(
    network: Network,
    formulation: str = "dc",
    mode: str = "preventive",
    tolerance: float = 0.01,
    max_iterations: int = 20,
    max_cuts_per_iteration: int = 100,
    corrective_ramp_window_min: float = 10.0,
    voltage_threshold: float = 0.01,
    contingency_rating: str = "rate-a",
    enforce_flowgates: bool = True,
    enforce_voltage_security: bool = True,
    lp_solver: Optional[str] = None,
    nlp_solver: Optional[str] = None,
    max_contingencies: int = 0,
    min_rate_a: float = 1.0,
    nr_max_iterations: int = 30,
    nr_convergence_tolerance: float = 1e-6,
    enable_screener: bool = True,
    screener_threshold_fraction: float = 0.9,
    screener_max_initial_contingencies: int = 500,
    warm_start: Optional[ScopfResult] = None,
) -> ScopfResult:
    """Solve Security-Constrained OPF (unified API).

    Dispatches to DC or AC formulation, preventive or corrective mode.

    Args:
        network: Power system network with generator cost curves.
        formulation: 'dc' (default) or 'ac'.
        mode: 'preventive' (default) or 'corrective'.
        tolerance: Violation tolerance in p.u. (default 0.01).
        max_iterations: Maximum constraint-generation iterations (default 20).
        max_cuts_per_iteration: Maximum cuts per iteration (default 100).
        corrective_ramp_window_min: Corrective ramp window in minutes (default 10.0).
        voltage_threshold: Voltage violation threshold in p.u. (AC only, default 0.01).
        contingency_rating: Thermal rating for post-contingency limits: 'rate-a' (default), 'rate-b', 'rate-c'.
        enforce_flowgates: Enforce flowgate/interface constraints (default True).
        enforce_voltage_security: Enforce post-contingency voltage limits in AC-SCOPF (default True).
        lp_solver: LP solver backend for DC ('highs' or 'gurobi'). Default: HiGHS.
        nlp_solver: NLP solver backend for AC ('ipopt'). Default: auto-detect.

    Returns:
        ScopfResult with base-case OPF, screening stats, and contingency metadata.
    """
    ...


def solve_dispatch(
    network: Network,
    request: dict[str, Any] | str | None = None,
    lp_solver: Optional[str] = None,
) -> DispatchResult:
    """Solve a canonical dispatch study from a JSON-like request payload."""
    ...


def analyze_n1_branch(
    network: Network,
    options: ContingencyOptions | None = None,
    on_progress: Callable[[int, int], None] | None = None,
) -> ContingencyAnalysis:
    """Compute N-1 branch contingency analysis.

    Args:
        network: Power system network.
        options: Contingency analysis options. If None, uses defaults.
            See :class:`ContingencyOptions` for all available parameters.
        on_progress: Optional callback invoked after each contingency is solved.
            Called with ``(n_done: int, n_total: int)``. Executes on rayon worker
            threads -- keep it lightweight (e.g. update a progress bar).

    Returns:
        ContingencyAnalysis with per-contingency results and summary statistics.
    """
    ...


def generate_breaker_contingencies(network: Network) -> list[Contingency]:
    """Generate breaker contingencies from a network's retained node-breaker topology.

    Creates one contingency per closed breaker in the network's
    ``NodeBreakerTopology``.  Each contingency opens one breaker and
    rebuilds the network before solving.

    Args:
        network: Network with retained node-breaker topology (e.g. from CGMES or PSS/E v35+).

    Returns:
        List of ``Contingency`` objects with ``switches`` populated.

    Raises:
        NetworkError: If the network has no retained node-breaker topology.
    """
    ...


def analyze_n2_branch(
    network: Network,
    options: ContingencyOptions | None = None,
) -> ContingencyAnalysis:
    """Compute N-2 simultaneous double branch contingency analysis.

    Generates all C(n,2) branch pairs. O(n^2) -- use screening='lodf' for large networks.

    Args:
        network: Power system network.
        options: Contingency analysis options. If None, uses defaults.
            See :class:`ContingencyOptions` for all available parameters.

    Returns:
        ContingencyAnalysis with all N-2 pair results.
    """
    ...


def analyze_n1_generator(
    network: Network,
    options: ContingencyOptions | None = None,
) -> ContingencyAnalysis:
    """Compute N-1 generator contingency analysis.

    For each in-service generator, removes it from service using the fast
    injection-vector path (no Y-bus rebuild).

    Args:
        network: Power system network.
        options: Contingency analysis options. If None, uses defaults.
            See :class:`ContingencyOptions` for all available parameters.

    Returns:
        ContingencyAnalysis with one result per in-service generator.
    """
    ...


def solve_corrective_dispatch(
    network: Network,
    contingency_analysis: ContingencyAnalysis,
) -> list[dict[str, object]]:
    """Solve corrective redispatch (SCRD) for contingencies with thermal violations.

    Args:
        network: Power system network (provides base dispatch and limits).
        contingency_analysis: Result from analyze_n1_branch() or analyze_n1_generator().

    Returns:
        List of dicts, one per contingency with thermal violations. Keys:
          - 'id' (str), 'status' (str), 'total_redispatch_mw' (float),
          - 'total_cost' (float), 'violations_resolved' (int),
          - 'unresolvable_violations' (int).
    """
    ...


class PreparedCorrectiveDispatchStudy:
    """Prepared DC sensitivity state for repeated corrective-dispatch runs."""

    def solve_corrective_dispatch(
        self,
        contingency_analysis: ContingencyAnalysis,
    ) -> list[dict[str, object]]:
        """Solve corrective redispatch using the prepared study state."""
        ...


def prepare_corrective_dispatch_study(
    network: Network,
) -> PreparedCorrectiveDispatchStudy:
    """Prepare reusable corrective-dispatch sensitivity state for one network."""
    ...


class ContingencyStudy:
    """Configured contingency study with optional cached analysis and corrective redispatch."""

    @property
    def kind(self) -> str:
        """Study family: ``'n1_branch'``, ``'n1_generator'``, or ``'n2_branch'``."""
        ...

    def analyze(self) -> ContingencyAnalysis:
        """Run the configured study and return the latest contingency analysis."""
        ...

    def solve_corrective_dispatch(self) -> list[dict[str, object]]:
        """Solve corrective redispatch for the latest contingency analysis."""
        ...


def n1_branch_study(
    network: Network,
    options: Optional[ContingencyOptions] = None,
) -> ContingencyStudy:
    """Build an N-1 branch contingency study for one network."""
    ...


def n1_generator_study(
    network: Network,
    options: Optional[ContingencyOptions] = None,
) -> ContingencyStudy:
    """Build an N-1 generator contingency study for one network."""
    ...


def n2_branch_study(
    network: Network,
    options: Optional[ContingencyOptions] = None,
) -> ContingencyStudy:
    """Build an N-2 branch contingency study for one network."""
    ...



def compute_lole(
    capacity_mw: NDArray[np.float64],
    forced_outage_rate: NDArray[np.float64],
    hourly_load_mw: NDArray[np.float64],
    step_mw: float = 1.0,
) -> LoleResult:
    """Compute LOLE (Loss of Load Expectation) for a generator fleet.

    Args:
        capacity_mw: Numpy array of generator capacities (MW).
        forced_outage_rate: Numpy array of forced outage rates [0, 1].
        hourly_load_mw: Numpy array of hourly load (MW), typically 8760 values.
        step_mw: COPT discretization step size (MW).

    Returns:
        LoleResult with lole_hours, lole_days, eue_mwh, hourly_lolp.
    """
    ...


def compute_elcc(
    capacity_mw: NDArray[np.float64],
    forced_outage_rate: NDArray[np.float64],
    hourly_load_mw: NDArray[np.float64],
    new_capacity_mw: float,
    new_for: float = 0.0,
    step_mw: float = 1.0,
    tolerance_mw: float = 0.5,
) -> ElccResult:
    """Compute ELCC (Effective Load Carrying Capability) of a new resource.

    Args:
        capacity_mw: Numpy array of existing generator capacities (MW).
        forced_outage_rate: Numpy array of existing generator FORs [0, 1].
        hourly_load_mw: Numpy array of hourly load (MW).
        new_capacity_mw: Capacity of the new resource (MW).
        new_for: Forced outage rate of the new resource [0, 1].
        step_mw: COPT discretization step size (MW).
        tolerance_mw: Bisection tolerance (MW).

    Returns:
        ElccResult with elcc_mw, elcc_fraction, lole_before, lole_after_addition.
    """
    ...


class PortfolioElccResult:
    """Result of portfolio ELCC computation."""

    @property
    def marginal_elcc_mw(self) -> NDArray[np.float64]:
        """Per-resource marginal ELCC (MW), in addition order."""
        ...
    @property
    def marginal_elcc_fraction(self) -> NDArray[np.float64]:
        """Per-resource marginal ELCC fraction (ELCC / nameplate)."""
        ...
    @property
    def resource_capacity_mw(self) -> NDArray[np.float64]:
        """Per-resource nameplate capacity (MW)."""
        ...
    @property
    def total_elcc_mw(self) -> float:
        """Total portfolio ELCC (MW) — sum of marginal ELCCs."""
        ...
    def __repr__(self) -> str: ...


def compute_portfolio_elcc(
    fleet_capacity_mw: NDArray[np.float64],
    fleet_for: NDArray[np.float64],
    hourly_load_mw: NDArray[np.float64],
    new_capacity_mw: NDArray[np.float64],
    new_for: NDArray[np.float64],
    step_mw: float = 1.0,
    tolerance_mw: float = 0.5,
) -> PortfolioElccResult:
    """Compute portfolio ELCC for multiple resources with diversity accounting.

    Resources are added sequentially; each resource's marginal ELCC is computed
    against the fleet including all previously added resources.

    Args:
        fleet_capacity_mw: Existing fleet generator capacities (MW).
        fleet_for: Existing fleet forced outage rates [0, 1].
        hourly_load_mw: Hourly system load (MW).
        new_capacity_mw: Per-resource nameplate capacities (MW).
        new_for: Per-resource forced outage rates [0, 1].
        step_mw: COPT discretization step (default 1.0).
        tolerance_mw: Bisection convergence tolerance (default 0.5).

    Returns:
        PortfolioElccResult with per-resource marginal ELCCs and total.
    """
    ...


def compute_lole_monte_carlo(
    capacity_mw: NDArray[np.float64],
    forced_outage_rate: NDArray[np.float64],
    hourly_load_mw: NDArray[np.float64],
    n_trials: int = 5000,
    seed: Optional[int] = None,
) -> MonteCarloLoleResult:
    """Compute LOLE via Monte Carlo state-sampling simulation.

    Args:
        capacity_mw: Numpy array of generator capacities (MW).
        forced_outage_rate: Numpy array of forced outage rates [0, 1].
        hourly_load_mw: Numpy array of hourly load (MW).
        n_trials: Number of Monte Carlo trials (default 5000).
        seed: Random seed for reproducibility (optional).

    Returns:
        MonteCarloLoleResult with lole_hours, lole_days, eue_mwh, confidence intervals.
    """
    ...


def compute_renewable_elcc(
    capacity_mw: NDArray[np.float64],
    forced_outage_rate: NDArray[np.float64],
    hourly_load_mw: NDArray[np.float64],
    capacity_factors: NDArray[np.float64],
    resource_name: str = "renewable",
    step_mw: float = 1.0,
    tolerance_mw: float = 0.5,
) -> RenewableElccResult:
    """Compute ELCC of a variable renewable resource using time-varying capacity factors.

    Args:
        capacity_mw: Numpy array of existing generator capacities (MW).
        forced_outage_rate: Numpy array of existing generator FORs [0, 1].
        hourly_load_mw: Numpy array of hourly load (MW).
        capacity_factors: Numpy array of hourly capacity factors [0, 1] for the renewable resource.
        resource_capacity_mw: Nameplate capacity of the renewable resource (MW).
        resource_name: Name of the resource (default "renewable").
        step_mw: COPT discretization step (MW).
        tolerance_mw: Bisection tolerance (MW).

    Returns:
        RenewableElccResult with elcc_mw, elcc_fraction, lole_before, resource info.
    """
    ...


def compute_multi_area_lole(
    areas: list[dict],
    transfer_limits: list[tuple[int, int, float]],
    step_mw: float = 1.0,
) -> MultiAreaLoleResult:
    """Compute multi-area LOLE with inter-area transfer limits.

    Args:
        areas: List of dicts, each with 'capacity_mw', 'forced_outage_rate', 'hourly_load_mw' arrays.
        transfer_limits: List of (from_area, to_area, limit_mw) tuples.
        step_mw: COPT discretization step (MW).

    Returns:
        MultiAreaLoleResult with per-area and system-wide LOLE.
    """
    ...


def simulate_sequential_mc(
    capacity_mw: NDArray[np.float64],
    forced_outage_rate: NDArray[np.float64],
    hourly_load_mw: NDArray[np.float64],
    n_years: int = 1000,
    mttr_hours: float = 50.0,
    seed: int = 42,
) -> SequentialMcResult:
    """Run sequential Monte Carlo reliability simulation with persistent outages.

    Args:
        capacity_mw: Numpy array of generator capacities (MW).
        forced_outage_rate: Numpy array of forced outage rates [0, 1].
        hourly_load_mw: Numpy array of hourly load (MW), one year (8760 values).
        n_years: Number of simulated years (default 1000).
        mttr_hours: Mean time to repair in hours (default 50.0).
        seed: Random seed (default 42).

    Returns:
        SequentialMcResult with lole_hours, lole_days, eue_mwh, n_years.
    """
    ...


def compute_storage_credit(
    power_mw: float,
    energy_mwh: float,
    rte: float = 0.85,
    forced_outage_rate: float = 0.02,
) -> float:
    """Compute capacity credit for a battery storage resource.

    Args:
        power_mw: Storage power rating (MW).
        energy_mwh: Storage energy capacity (MWh).
        rte: Round-trip efficiency [0, 1] (default 0.85).
        forced_outage_rate: Forced outage rate [0, 1] (default 0.02).

    Returns:
        Capacity credit in MW.
    """
    ...


def solve_expansion(
    network: Network,
    candidate_buses: list[int],
    candidate_pmax: list[float],
    candidate_capex: list[float],
    candidate_mc: list[float],
    reserve_margin: float = 0.15,
    peak_load_override_mw: float = 0.0,
    existing_cap_override_mw: float = 0.0,
    annual_hours: float = 8760.0,
    lp_solver: Optional[str] = None,
) -> ExpansionSolution:
    """Solve capacity expansion (least-cost generation investment).

    Args:
        network: Power system network.
        candidate_buses: List of bus numbers for candidate generators.
        candidate_pmax: List of max capacity (MW) per candidate.
        candidate_capex: List of annualized capital cost ($/MW/year) per candidate.
        candidate_mc: List of marginal cost ($/MWh) per candidate.
        reserve_margin: Planning reserve margin (fraction, default 0.15 = 15%).
        peak_load_override_mw: Override peak load for reserve margin calc (0.0 = use network).
        existing_cap_override_mw: Override existing capacity for reserve margin calc (0.0 = use network).
        annual_hours: Hours per year for operating cost scaling.
        lp_solver: LP solver backend ('highs' or 'gurobi'). Default: HiGHS.

    Returns:
        ExpansionSolution with investments, total_annual_cost, total_new_capacity_mw.
    """
    ...


# ---------------------------------------------------------------------------
# Voltage stability functions
# ---------------------------------------------------------------------------
def compute_voltage_stress(
    network: Network,
    options: Optional[VoltageStressOptions] = None,
) -> VoltageStressResult:
    """Compute base-case voltage stress for a network.

    Runs AC Newton-Raphson power flow and returns the exact/proxy result shape
    used by contingency analysis. By default this uses ``VoltageStressOptions()``,
    which selects ``mode='exact_l_index'``.
    """
    ...



# ---------------------------------------------------------------------------
# Probabilistic functions
# ---------------------------------------------------------------------------
# ---------------------------------------------------------------------------
# Frequency response
# ---------------------------------------------------------------------------



# ---------------------------------------------------------------------------
# Governor-model SFR (TGOV1 / GGOV1)
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Modal / small-signal screening
# ---------------------------------------------------------------------------

def solve_electromechanical_modes(network: Network) -> ModalResult:
    """Screen electromechanical oscillatory modes using the GENCLS classical model.

    Computes electromechanical modes of the linearized state matrix around
    the AC power flow operating point. Uses GENCLS (classical machine) model —
    does NOT include exciters, PSS, or full DAE linearization.

    Args:
        network: Power system network (must have generators).

    Returns:
        ModalResult with oscillatory modes and stability assessment.
    """
    ...


# ---------------------------------------------------------------------------
# Harmonics functions
# ---------------------------------------------------------------------------

class IterativeHarmonicResult:
    """Result of iterative nonlinear harmonic power flow (solve_iterative_harmonic_pf)."""

    @property
    def converged(self) -> bool: ...
    @property
    def iterations(self) -> int: ...
    @property
    def max_mismatch(self) -> float: ...
    @property
    def convergence_history(self) -> NDArray[np.float64]: ...
    @property
    def n_saturating_xfmrs(self) -> int: ...
    @property
    def harmonic_orders(self) -> list[int]: ...
    @property
    def bus_harmonic_voltages(self) -> list[list[float]]:
        """Per-bus harmonic voltage magnitudes: bus_harmonic_voltages[bus][h_idx]."""
        ...
    def to_dataframe(self) -> dict[str, Any]:
        """Return dict for pd.DataFrame. Columns: bus_number, V_h<N> for each harmonic order."""
        ...


def solve_iterative_harmonic_pf(
    network: Network,
    harmonic_orders: Optional[list[int]] = None,
    saturation_toml: Optional[str] = None,
    source_buses: Optional[list[int]] = None,
    source_magnitude: float = 0.1,
    max_iter: int = 50,
    tolerance: float = 1e-6,
    anderson_depth: int = 5,
    dc_flux_offsets: Optional[dict[int, float]] = None,
    frequency_hz: float = 60.0,
) -> IterativeHarmonicResult:
    """Solve iterative nonlinear harmonic power flow with transformer saturation.

    Handles voltage-dependent saturation currents, converter commutation overlap,
    and frequency-dependent core losses. Uses fixed-point iteration with Anderson
    acceleration. Fundamental voltages (h=1) are pinned from the power flow solution.

    Args:
        network: Power system network model.
        harmonic_orders: Orders to solve (default: [1, 3, 5, 7, 9, 11, 13]).
        saturation_toml: Path to TOML file with saturation curves and converters.
        source_buses: Bus numbers for linear harmonic source injection.
        source_magnitude: Magnitude of linear harmonic sources (pu, default: 0.1).
        max_iter: Maximum outer-loop iterations (default: 50).
        tolerance: Convergence tolerance on voltage change (pu, default: 1e-6).
        anderson_depth: Anderson acceleration window size; 0 to disable (default: 5).
        dc_flux_offsets: DC flux offset per branch index (pu) for GIC studies.
        frequency_hz: System frequency in Hz (default: 60.0).

    Returns:
        IterativeHarmonicResult with converged voltages and convergence info.
    """
    ...


def solve_harmonic_pf(
    network: Network,
    bus_numbers: list[int],
    harmonic_orders: Optional[list[int]] = None,
    source_magnitude: float = 0.1,
) -> HarmonicResult:
    """Run a harmonic power flow analysis and return detailed per-bus results.

    Args:
        network: Fundamental-frequency power system network.
        bus_numbers: External bus numbers where harmonic current sources are injected.
        harmonic_orders: Harmonic orders to inject (default: [5, 7, 11, 13]).
        source_magnitude: Per-unit current magnitude for each order (default: 0.1 p.u.).

    Returns:
        HarmonicResult with full per-bus harmonic voltage and IEEE 519 compliance.
    """
    ...


def check_iec61000(
    base_kv: float,
    thd_pct: float,
    ihd_per_order: list[tuple[int, float]],
) -> Iec61000Report:
    """Check IEC 61000-3-6 compliance.

    Args:
        base_kv: Bus voltage level in kV.
        thd_pct: Total harmonic distortion (%).
        ihd_per_order: Individual harmonic distortion: list of (order, ihd_pct).

    Returns:
        Iec61000Report with compliance details.
    """
    ...


def list_device_spectra() -> list[str]:
    """List available harmonic device spectrum names (e.g. 'vfd_6pulse', 'led_lighting')."""
    ...


def get_device_spectrum(name: str) -> list[tuple[int, float, float]]:
    """Get a device spectrum by name.

    Args:
        name: Device spectrum name from list_device_spectra().

    Returns:
        List of (order, magnitude_pct, phase_deg) entries, or empty if name not found.
    """
    ...


def analyze_n1_harmonic(
    network: Network,
    bus_numbers: list[int],
    harmonic_orders: Optional[list[int]] = None,
    source_magnitude: float = 0.1,
    thd_limit_pct: float = 5.0,
) -> HarmonicContingencyResult:
    """Run N-1 harmonic contingency analysis.

    For each in-service branch, re-solves harmonic PF with branch removed and
    reports THD changes, worst contingency per bus, and new violations.

    Args:
        network: Power system network.
        bus_numbers: Buses where harmonic sources are injected.
        harmonic_orders: Harmonic orders to analyze (default: [5, 7, 11, 13]).
        source_magnitude: Per-unit current magnitude per order (default: 0.1).
        thd_limit_pct: THD limit for violation detection (default: 5.0%).

    Returns:
        HarmonicContingencyResult with per-bus and branch sensitivity data.
    """
    ...


def build_fdne(
    network: Network,
    boundary_buses: list[int],
    f_min: float = 60.0,
    f_max: float = 3000.0,
    n_points: int = 100,
    n_poles: int = 8,
) -> list[tuple[int, RationalModel]]:
    """Build FDNE rational models from a network frequency sweep.

    Args:
        network: Power system network.
        boundary_buses: Bus numbers at which to build FDNEs.
        f_min: Minimum sweep frequency in Hz (default: 60.0).
        f_max: Maximum sweep frequency in Hz (default: 3000.0).
        n_points: Number of frequency sweep points (default: 100).
        n_poles: Number of poles for the rational fit (default: 8).

    Returns:
        List of (bus_number, RationalModel) pairs.
    """
    ...


def vector_fit(
    frequencies: list[float],
    z_real: list[float],
    z_imag: list[float],
    n_poles: int = 8,
    max_iter: int = 20,
    enforce_passivity: bool = False,
) -> RationalModel:
    """Fit a rational function to frequency-domain impedance data via Vector Fitting.

    Args:
        frequencies: Frequency points in Hz.
        z_real: Real part of impedance at each frequency.
        z_imag: Imaginary part of impedance at each frequency.
        n_poles: Number of poles (default: 8).
        max_iter: Maximum iterations (default: 20).
        enforce_passivity: Enforce Re(Z(jw)) >= 0 (default: False).

    Returns:
        Fitted RationalModel.
    """
    ...


def check_bdew_2008(
    base_kv: float,
    ihd_per_order: list[tuple[int, float]],
) -> BdewReport:
    """Check BDEW 2008 MV harmonic compliance.

    Args:
        base_kv: Bus voltage level in kV.
        ihd_per_order: Individual harmonic distortion: list of (order, ihd_pct).

    Returns:
        BdewReport with per-order compliance.
    """
    ...


def compute_emission_allocation(
    planning_level_pct: float,
    customer_mva: float,
    total_hosting_mva: float,
    harmonic_order: int,
) -> float:
    """Compute IEC 61000-3-6 emission allocation for a customer.

    Args:
        planning_level_pct: Planning level at the PCC (%).
        customer_mva: Customer's agreed power (MVA).
        total_hosting_mva: Total hosting capacity at the PCC (MVA).
        harmonic_order: Harmonic order (determines summation exponent alpha).

    Returns:
        Allocated emission limit (%).
    """
    ...


def solve_harmonic_3ph(
    network: Network,
    bus_numbers: list[int],
    source_magnitude: float = 0.1,
) -> Harmonic3phResult:
    """Solve three-phase unbalanced harmonic power flow.

    Converts the balanced network to three-phase and solves with per-phase
    harmonic current injections.

    Args:
        network: Fundamental-frequency network (auto-converted to 3-phase).
        bus_numbers: Buses where harmonic sources are injected (balanced across phases).
        harmonic_orders: Harmonic orders (default: [5, 7, 11, 13]).
        source_magnitude: Per-unit current magnitude per order per phase (default: 0.1).

    Returns:
        Harmonic3phResult with per-phase THD and VUF.
    """
    ...


# ---------------------------------------------------------------------------
# GIC functions
# ---------------------------------------------------------------------------

def compute_gic(
    network: Network,
    efield_v_per_km: float = 5.0,
    azimuth_deg: float = 0.0,
) -> GicResult:
    """Compute GIC for a network driven by a uniform E-field (network-based).

    Constructs a minimal GIC network from the AC network topology and solves
    for transformer GIC and reactive power impacts.

    Args:
        network: Power system network (buses become substations).
        efield_v_per_km: Geomagnetic E-field magnitude (V/km).
        azimuth_deg: E-field azimuth (degrees clockwise from north).

    Returns:
        GicResult with transformer_gic_amps, total_q_demand_mvar, etc.
    """
    ...


def compute_gic_parametric(
    n_substations: int,
    line_length_km: float,
    efield_v_per_km: float,
    line_resistance_ohm_per_km: float = 0.05,
    efield_angle_deg: float = 0.0,
    ground_resistance_ohm: float = 0.5,
    k_factor: float = 1.18,
) -> GicStudyResult:
    """Build a minimal GIC network from scalar parameters and compute GIC.

    Convenience wrapper for quick GIC screening without building a full GicNetwork.

    Args:
        n_substations: Number of substations (>= 2).
        line_length_km: Average transmission line length (km).
        efield_v_per_km: Geomagnetic E-field magnitude (V/km). Typical storm: 1-10 V/km.
        line_resistance_ohm_per_km: DC line resistance per km (default: 0.05 Ω/km).
        efield_angle_deg: E-field direction from geographic north (degrees).
        ground_resistance_ohm: Substation earth resistance (Ω).
        k_factor: Transformer K-factor for reactive power absorption (MVAr/A).

    Returns:
        GicStudyResult with NERC TPL-007 risk classification and detailed results.
    """
    ...


# ---------------------------------------------------------------------------
# SSR function
# ---------------------------------------------------------------------------

def analyze_ssr(
    series_compensation_pct: float,
    system_reactance_pu: float,
    generator_inertia_h: float,
    n_torsional_modes: int = 3,
    f0_hz: float = 60.0,
) -> SsrResult:
    """Run a complete SSR analysis on a series-compensated system.

    Args:
        series_compensation_pct: Series capacitor compensation level (0-100%).
        system_reactance_pu: Total series line reactance X_L in per-unit.
        generator_inertia_h: Generator inertia constant H in seconds.
        n_torsional_modes: Number of shaft mass segments (default 3).
        f0_hz: Nominal system frequency in Hz.

    Returns:
        SsrResult with resonance frequencies, damping, and SSR risk flags.
    """
    ...


# ---------------------------------------------------------------------------
# Arc flash functions
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Motor functions
# ---------------------------------------------------------------------------

def analyze_motor_start(
    motor: InductionMotor,
    v_bus_pu: float = 1.0,
    inertia_h: float = 1.0,
    load_torque_factor: float = 0.8,
    t_max_s: float = 10.0,
) -> MotorStartResult:
    """Simulate a motor start transient.

    Args:
        motor: InductionMotor parameters.
        v_bus_pu: Terminal bus voltage during the start (per-unit).
        inertia_h: Combined motor + load inertia constant H (seconds).
        load_torque_factor: Load torque at synchronous speed as fraction of rated torque.
        t_max_s: Maximum simulation time (s).

    Returns:
        MotorStartResult with start_success, start_time_s, min_voltage_pu, peak_current_pu.
    """
    ...


def compute_motor_operating_point(
    rated_kw: float,
    rated_kv: float,
    r1: float,
    x1: float,
    r2: float,
    x2: float,
    xm: float,
    slip: float = 0.03,
) -> MotorOperatingPoint:
    """Analyze an induction motor operating point using the T-equivalent circuit.

    Args:
        rated_kw: Rated shaft output power (kW).
        rated_kv: Rated line-to-line terminal voltage (kV).
        r1: Stator resistance (per-unit on motor rated kVA base).
        x1: Stator leakage reactance (per-unit).
        r2: Rotor resistance referred to stator (per-unit).
        x2: Rotor leakage reactance referred to stator (per-unit).
        xm: Magnetizing reactance (per-unit).
        slip: Per-unit slip (0 < slip <= 1). Default: 0.03 (3% full-load slip).

    Returns:
        MotorOperatingPoint with torque_nm, power_mech_kw, efficiency_pct, power_factor.
    """
    ...


def compute_motor_torque_speed(
    rated_kw: float,
    rated_kv: float,
    r1: float,
    x1: float,
    r2: float,
    x2: float,
    xm: float,
    n_points: int = 50,
) -> tuple[NDArray[np.float64], NDArray[np.float64]]:
    """Compute the torque-speed curve for an induction motor.

    Args:
        rated_kw, rated_kv, r1, x1, r2, x2, xm: Motor parameters (same as compute_motor_operating_point).
        n_points: Number of curve points.

    Returns:
        (speeds_rpm, torques_nm): Tuple of 1-D numpy arrays.
    """
    ...


# ---------------------------------------------------------------------------
# Resiliency
# ---------------------------------------------------------------------------

def analyze_resiliency(network: Network) -> ResiliencyResult:
    """Perform a full grid resiliency analysis (IEEE 1366 reliability indices).

    Computes component criticality rankings, N-2 screening, and reliability indices
    SAIDI, SAIFI, CAIDI, ASAI.

    Args:
        network: Power system network.

    Returns:
        ResiliencyResult with SAIDI, SAIFI, CAIDI, ASAI, and critical branch list.
    """
    ...


# ---------------------------------------------------------------------------
# Line parameters
# ---------------------------------------------------------------------------

def compute_line_parameters(
    r_dc_ohm_per_km: float,
    gmr_m: float,
    outside_radius_m: float,
    phase_spacings_m: list[float],
    length_km: float,
    freq_hz: float = 60.0,
    earth_resistivity: float = 100.0,
) -> LineParametersResult:
    """Compute transmission line parameters from conductor and geometry data (Carson's equations).

    Args:
        r_dc_ohm_per_km: Conductor DC resistance at 25°C (Ω/km).
        gmr_m: Geometric Mean Radius of the conductor (m).
        outside_radius_m: Overall conductor outside radius (m).
        phase_spacings_m: Three horizontal phase spacings [x_a, x_b, x_c] in meters.
        length_km: Line length in km.
        freq_hz: System frequency (Hz).
        earth_resistivity: Earth resistivity (Ω·m).

    Returns:
        LineParametersResult with positive- and zero-sequence parameters.
    """
    ...



# ---------------------------------------------------------------------------
# State estimation
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Three-phase power flow
# ---------------------------------------------------------------------------

def solve_3ph(network: ThreePhaseNetwork) -> ThreePhaseSolution:
    """Solve a three-phase unbalanced power flow.

    Args:
        network: ThreePhaseNetwork object.

    Returns:
        ThreePhaseSolution with per-phase voltages, VUF, and convergence info.
    """
    ...


# ---------------------------------------------------------------------------
# Protection relay functions
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Distribution functions
# ---------------------------------------------------------------------------

def solve_distribution(network: DistNetwork) -> DistSolution:
    """Solve a distribution (radial) power flow using Backward-Forward Sweep.

    Args:
        network: DistNetwork radial distribution network.

    Returns:
        DistSolution with bus voltages, branch flows, and loss totals.
    """
    ...


def parse_dss(path: str) -> ThreePhaseDistNetwork:
    """Parse an OpenDSS master .dss file into a three-phase distribution network.

    Args:
        path: Path to OpenDSS master .dss file (e.g., "IEEE13Nodeckt.dss").

    Returns:
        ThreePhaseDistNetwork ready for solve_3phase_bfs().
    """
    ...


def solve_3phase_bfs(
    network: ThreePhaseDistNetwork,
    max_iter: int = 50,
    tolerance: float = 1e-6,
) -> ThreePhaseBfsResult:
    """Solve three-phase unbalanced power flow using Backward-Forward Sweep.

    Args:
        network: Three-phase distribution network (from parse_dss()).
        max_iter: Maximum BFS iterations (default 50).
        tolerance: Voltage convergence tolerance in per-unit (default 1e-6).

    Returns:
        ThreePhaseBfsResult with per-phase voltages, losses, and VUF.
    """
    ...


def compute_hosting_capacity(network: DistNetwork, bus_idx: int) -> float:
    """Compute the hosting capacity at a given bus index.

    Binary search over DER injection levels at bus_idx, returning the maximum
    real power (kW) that can be connected before a voltage or thermal constraint
    is violated.

    Args:
        network: Radial distribution network.
        bus_idx: 0-based bus index at which DER is to be sited.

    Returns:
        Hosting capacity in kW.
    """
    ...


def ieee13_test_network() -> DistNetwork:
    """Return the IEEE 13-bus test feeder as a DistNetwork."""
    ...


def ieee34_test_network() -> DistNetwork:
    """Return the IEEE 34-bus test feeder as a DistNetwork."""
    ...


def ieee37_test_network() -> DistNetwork:
    """Return the IEEE 37-bus test feeder as a DistNetwork."""
    ...


def ieee123_test_network() -> DistNetwork:
    """Return the IEEE 123-bus test feeder as a DistNetwork."""
    ...


def compute_stochastic_hc(
    network: DistNetwork,
    n_trials: int = 100,
    load_sigma_pct: float = 0.10,
    der_sigma_pct: float = 0.15,
    v_min: float = 0.95,
    v_max: float = 1.05,
    seed: int = 42,
) -> StochasticHcResult:
    """Compute per-bus stochastic Monte Carlo DER hosting capacity.

    Args:
        network: Radial distribution network.
        n_trials: Number of Monte Carlo trials.
        load_sigma_pct: Load standard deviation as fraction of nominal.
        der_sigma_pct: Existing DER standard deviation as fraction of nominal.
        v_min: Minimum voltage limit in per-unit.
        v_max: Maximum voltage limit in per-unit.
        seed: Random seed for reproducibility.

    Returns:
        StochasticHcResult with mean, p10, p50, p90 per bus.
    """
    ...


# ---------------------------------------------------------------------------
# Wave 4/5 functions
# ---------------------------------------------------------------------------

def compute_interharmonic_voltages(
    network: Network,
    injection_buses: list[int],
    frequencies_hz: list[float],
    magnitudes_pu: list[float],
    phases_rad: Optional[list[float]] = None,
    base_frequency_hz: float = 60.0,
) -> list[InterharmonicResult]:
    """Compute interharmonic voltage distortion across the network.

    Args:
        network: Power system network.
        injection_buses: Bus numbers where spectral current sources are connected.
        frequencies_hz: Frequency (Hz) of each injection.
        magnitudes_pu: Current injection magnitude (per-unit) for each source.
        phases_rad: Phase angle (radians) for each source. Default: all zeros.
        base_frequency_hz: System fundamental frequency (Hz).

    Returns:
        List of InterharmonicResult, one per injection.
    """
    ...


def compute_voltage_risk(
    voltage_samples: list[float],
    n_buses: int,
    v_min: float = 0.95,
    v_max: float = 1.05,
) -> VoltageRiskResult:
    """Compute probabilistic voltage violation risk from Monte Carlo voltage samples.

    Args:
        voltage_samples: Flat list of voltage magnitude samples (p.u.), row-major.
                         Length must equal n_samples * n_buses.
        n_buses: Number of buses (columns).
        v_min: Lower voltage limit (p.u.).
        v_max: Upper voltage limit (p.u.).

    Returns:
        VoltageRiskResult with per-bus and system-wide violation probabilities.
    """
    ...


# ---------------------------------------------------------------------------
# Feeder coordination study
# ---------------------------------------------------------------------------

class FeederCoordResult:
    """Result of a feeder coordination study for one relay pair."""

    relay_upstream: str
    relay_downstream: str
    fault_current_pu: float
    t_upstream_s: float
    t_downstream_s: float
    cti_s: float
    coordinated: bool
    violation: str


def analyze_feeder_coordination(
    relay_specs: list[tuple[str, str, float, float, float]],
    fault_currents: list[float],
    cti_min: float = 0.2,
) -> list[FeederCoordResult]:
    """Analyze coordination for a chain of OC relays on a radial feeder."""
    ...


def optimize_relay_tds(
    relay_specs: list[tuple[str, str, float, float, float]],
    fault_currents: list[float],
    cti_min: float = 0.2,
) -> dict:
    """Optimize TDS settings for a chain of OC relays on a radial feeder."""
    ...


def analyze_ct_saturation(
    ct_ratio: float,
    burden_va: float,
    secondary_resistance_ohm: float,
    lead_resistance_ohm: float,
    knee_voltage_v: float,
    accuracy_class: str,
    fault_current_primary_a: float,
    dc_offset_factor: float = 0.0,
) -> dict:
    """Analyze CT saturation for a given fault condition (IEC 60044-1)."""
    ...


# ---------------------------------------------------------------------------
# Dynamics simulation
# ---------------------------------------------------------------------------

class _DynamicModel:
    """Internal package helper for parsed PSS/E dynamic model data."""

    def __init__(self) -> None: ...
    @property
    def generator_count(self) -> int: ...
    @property
    def exciter_count(self) -> int: ...
    @property
    def governor_count(self) -> int: ...
    @property
    def pss_count(self) -> int: ...
    @property
    def load_count(self) -> int: ...
    @property
    def facts_count(self) -> int: ...
    @property
    def unknown_record_count(self) -> int: ...
    def coverage(self) -> tuple[int, int, float]: ...
    def __repr__(self) -> str: ...


def _io_psse_dyr_load(path: str) -> _DynamicModel:
    """Internal package helper for loading a PSS/E DYR file."""
    ...


def _io_psse_dyr_loads(content: str) -> _DynamicModel:
    """Internal package helper for loading PSS/E DYR content from a string."""
    ...


def _io_psse_dyr_save(dyn_model: _DynamicModel, path: str) -> None:
    """Internal package helper for saving a PSS/E DYR file."""
    ...


def _io_psse_dyr_dumps(dyn_model: _DynamicModel) -> str:
    """Internal package helper for serializing a PSS/E DYR model."""
    ...


def _compose_merge_networks(
    net1: Network,
    net2: Network,
    tie_buses: list[tuple[int, int]] | None = None,
) -> Network:
    """Internal package helper for combining two networks."""
    ...


# ---------------------------------------------------------------------------
# Parameter sweep
# ---------------------------------------------------------------------------

class SweepResult:
    """A single scenario result from a parameter sweep."""

    @property
    def name(self) -> str:
        """Scenario name."""
        ...
    @property
    def converged(self) -> bool:
        """Whether the solver converged."""
        ...
    @property
    def solution(self) -> Optional[AcPfResult]:
        """The power flow solution, or None if the solve failed."""
        ...
    @property
    def error(self) -> Optional[str]:
        """Error message if the solve failed, or None."""
        ...
    def __repr__(self) -> str: ...

class SweepResults:
    """Results from a parameter sweep (collection of scenario results)."""

    @property
    def results(self) -> list[SweepResult]:
        """List of all scenario results."""
        ...
    def to_dataframe(self) -> pd.DataFrame:
        """Summary DataFrame with columns: name, converged, iterations,
        max_vm, min_vm, total_losses_mw, solve_time_secs."""
        ...
    def __len__(self) -> int: ...
    def __getitem__(self, idx: int) -> SweepResult: ...
    def __repr__(self) -> str: ...

def parameter_sweep(
    network: Network,
    scenarios: list[tuple[str, list[tuple]]],
    solver: str = "acpf",
    on_progress: Optional[Callable[[int, int], None]] = None,
) -> SweepResults:
    """Run a parameter sweep: solve multiple power flow scenarios in parallel.

    Each scenario clones the base network, applies a list of modifications,
    and solves using the specified solver. Scenarios execute in parallel
    via Rust threads (rayon), with the Python GIL released.

    Args:
        network:   Base power system network (not modified).
        scenarios: List of ``(name, modifications)`` tuples. Each modification
                   is a tuple ``(method_name, *args)``.
        solver:    ``"acpf"``, ``"dcpf"``, or ``"fdpf"``.
        on_progress: Optional callback invoked after each scenario completes.
            Called with ``(n_done: int, n_total: int)``. Keep it lightweight.

    Returns:
        SweepResults collection with per-scenario solutions.

    Raises:
        ValueError: If ``solver`` is not one of the supported solvers.
        ValueError: If a modification method name is unknown.
    """
    ...

# ---------------------------------------------------------------------------
# Pure-Python functions (audit, batch, contingency I/O)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Inertia estimation
# ---------------------------------------------------------------------------

class GeneratorInertia:
    """Per-generator inertia breakdown."""

    @property
    def bus(self) -> int:
        """External bus number."""
        ...
    @property
    def h_seconds(self) -> float:
        """Generator inertia constant H (seconds)."""
        ...
    @property
    def mbase_mva(self) -> float:
        """Generator MVA base."""
        ...
    @property
    def fuel_type(self) -> str:
        """Fuel type string."""
        ...
    @property
    def inertia_contribution_mws(self) -> float:
        """Inertia contribution (MW-seconds)."""
        ...
    def __repr__(self) -> str: ...


class InertiaResult:
    """System-wide inertia estimation result."""

    @property
    def h_system(self) -> float:
        """Aggregate system inertia constant (seconds)."""
        ...
    @property
    def total_online_mva(self) -> float:
        """Total online generation capacity (MVA)."""
        ...
    @property
    def inertia_by_fuel_type(self) -> dict[str, float]:
        """Inertia contribution by fuel type (MWs)."""
        ...
    @property
    def per_generator(self) -> list[GeneratorInertia]:
        """Per-generator inertia breakdown."""
        ...
    def to_dataframe(self) -> pd.DataFrame:
        """DataFrame with one row per generator. Columns: bus, h_seconds, mbase_mva, fuel_type, contribution_mws."""
        ...
    def __repr__(self) -> str: ...


def compute_inertia(
    network: Network,
    dyn_model: Optional[_DynamicModel] = None,
) -> InertiaResult:
    """Estimate system inertia from the network and optional dynamic model.

    Uses generator H constants from the dynamic model when available,
    falling back to fuel-type defaults.

    Args:
        network: Power system network.
        dyn_model: Optional parsed dynamic model from ``surge.io.psse.dyr.load()``.

    Returns:
        InertiaResult with system H, per-generator breakdown, and fuel-type totals.
    """
    ...


# ---------------------------------------------------------------------------
# Cascade analysis (Zone-3 relay cascade + OPA Monte Carlo)
# ---------------------------------------------------------------------------

class CascadeOptions:
    """Options for Zone-3 relay cascade simulation."""

    def __init__(
        self,
        z3_pickup_fraction: float = 0.8,
        z3_delay_s: float = 1.0,
        max_cascade_levels: int = 5,
        blackout_fraction: float = 0.5,
        thermal_rating: Optional[str] = None,
    ) -> None:
        """Create cascade simulation options.

        Args:
            z3_pickup_fraction: Zone 3 pickup fraction of thermal rating (default 0.8).
            z3_delay_s: Zone 3 trip delay in seconds (default 1.0).
            max_cascade_levels: Maximum cascade depth before stopping (default 5).
            blackout_fraction: Load-interruption fraction to declare blackout (default 0.5).
            thermal_rating: Rating tier: 'rate_a' (default), 'rate_b', or 'rate_c'.
        """
        ...

    @property
    def z3_pickup_fraction(self) -> float: ...
    @property
    def z3_delay_s(self) -> float: ...
    @property
    def max_cascade_levels(self) -> int: ...
    @property
    def blackout_fraction(self) -> float: ...
    @property
    def thermal_rating(self) -> str:
        """Rating tier: 'rate_a', 'rate_b', or 'rate_c'."""
        ...
    def __repr__(self) -> str: ...


class CascadeEvent:
    """A single trip event in a relay cascade sequence."""

    @property
    def cascade_level(self) -> int:
        """Cascade level at which this trip occurred (0 = initiating event)."""
        ...
    @property
    def branch_index(self) -> int:
        """Internal 0-based index of the tripped branch."""
        ...
    @property
    def branch_label(self) -> str:
        """Human-readable branch label ('from_bus->to_bus')."""
        ...
    @property
    def flow_mw_before(self) -> float:
        """Branch flow in MW immediately before the trip."""
        ...
    @property
    def rating_mw(self) -> float:
        """Thermal rating of the tripped branch in MW."""
        ...
    @property
    def time_s(self) -> float:
        """Simulation time in seconds when the trip occurred."""
        ...
    @property
    def cause(self) -> str:
        """Cause of the trip: 'initial', 'zone3_relay', or 'zone2_relay'."""
        ...
    def __repr__(self) -> str: ...


class CascadeResult:
    """Result of a Zone-3 relay cascade simulation for one initiating contingency."""

    @property
    def initiating_branch(self) -> int:
        """Internal index of the branch that initiated the cascade."""
        ...
    @property
    def events(self) -> list[CascadeEvent]:
        """Ordered list of trip events from the cascade sequence."""
        ...
    @property
    def cascade_depth(self) -> int:
        """Depth of the cascade (number of levels beyond the initiating event)."""
        ...
    @property
    def total_load_interrupted_mw(self) -> float:
        """Total load interrupted in MW."""
        ...
    @property
    def blackout(self) -> bool:
        """True if load interrupted exceeds the blackout fraction threshold."""
        ...
    def __repr__(self) -> str: ...


class OpaOptions:
    """Options for OPA Monte Carlo cascading failure simulation."""

    def __init__(
        self,
        beta: float = 2.0,
        max_steps: int = 100,
        n_trials: int = 1000,
        seed: Optional[int] = None,
        thermal_rating: Optional[str] = None,
    ) -> None:
        """Create OPA cascade simulation options.

        Args:
            beta: Overload-to-trip probability exponent (default 2.0).
            max_steps: Maximum simulation steps per trial (default 100).
            n_trials: Number of Monte Carlo trials (default 1000).
            seed: Random seed for reproducibility (default None = fixed seed).
            thermal_rating: Rating tier: 'rate_a' (default), 'rate_b', or 'rate_c'.
        """
        ...

    @property
    def beta(self) -> float: ...
    @property
    def max_steps(self) -> int: ...
    @property
    def n_trials(self) -> int: ...
    @property
    def seed(self) -> Optional[int]: ...
    @property
    def thermal_rating(self) -> str:
        """Rating tier: 'rate_a', 'rate_b', or 'rate_c'."""
        ...
    def __repr__(self) -> str: ...


class OpaCascadeResult:
    """Aggregate results from OPA Monte Carlo cascading failure simulation."""

    @property
    def mean_load_shed_mw(self) -> float:
        """Mean load shed in MW across all trials."""
        ...
    @property
    def std_load_shed_mw(self) -> float:
        """Standard deviation of load shed in MW."""
        ...
    @property
    def blackout_probability(self) -> float:
        """Probability of a large cascade (shed >= 50% of total load)."""
        ...
    @property
    def load_shed_cdf(self) -> list[tuple[float, float]]:
        """Empirical CDF of load-shed fraction.

        List of (load_shed_fraction, cumulative_probability) tuples,
        sorted by load_shed_fraction ascending.
        """
        ...
    @property
    def critical_branches(self) -> list[tuple[int, float]]:
        """Most critical branches ranked by expected load shed contribution.

        List of (branch_index, expected_load_shed_mw) tuples, sorted
        descending by expected load shed. Capped at 20 entries.
        """
        ...
    def __repr__(self) -> str: ...


def analyze_cascade(
    network: Network,
    initiating_branch: int,
    options: Optional[CascadeOptions] = None,
) -> CascadeResult:
    """Simulate a Zone-3 relay cascade for a single initiating branch outage.

    Uses a prepared DC model and single-outage LODF columns to screen cascade
    progression. This is a first-order relay-cascade approximation, not a full
    topology rebuild after every trip.

    Args:
        network: Power system network.
        initiating_branch: 0-based index of the branch to outage.
        options: CascadeOptions (default: z3_pickup=0.8, delay=1s, max_levels=5).

    Returns:
        CascadeResult with the full cascade event sequence.

    Raises:
        SurgeError: If relay-cascade preparation or simulation fails.
    """
    ...


def analyze_cascade_screening(
    network: Network,
    options: Optional[CascadeOptions] = None,
    top_n: int = 10,
) -> list[CascadeResult]:
    """Screen all branches for cascade risk and return the most severe results.

    Reuses one prepared relay-cascade model for every in-service branch with a
    valid thermal rating. Results are sorted by severity
    (cascade depth descending, then load interrupted descending).

    Args:
        network: Power system network.
        options: CascadeOptions (default settings if None).
        top_n: Return only the top N most severe cascades (default 10).

    Returns:
        List of CascadeResult, sorted most-severe first, capped at top_n.

    Raises:
        SurgeError: If relay-cascade preparation or simulation fails.
    """
    ...


def analyze_opa_cascade(
    network: Network,
    initial_outages: list[int],
    options: Optional[OpaOptions] = None,
) -> OpaCascadeResult:
    """Run OPA Monte Carlo cascading failure simulation.

    Performs probabilistic cascade analysis using the OPA model. Internally
    solves DC power flow, computes PTDF/LODF, then runs n_trials Monte
    Carlo trials with probabilistic relay tripping.

    Args:
        network: Power system network.
        initial_outages: List of 0-based branch indices to outage at time 0.
        options: OpaOptions (default: beta=2.0, max_steps=100, n_trials=1000).

    Returns:
        OpaCascadeResult with mean/std load shed, blackout probability,
        empirical CDF, and critical branch ranking.

    Raises:
        SurgeError: If inputs are invalid or DC power flow fails.
    """
    ...

# ---------------------------------------------------------------------------
# Built-in test networks (IEEE benchmark cases)
# ---------------------------------------------------------------------------

def case9() -> Network:
    """IEEE 9-bus (WSCC 3-machine system).

    The classic Anderson & Fouad 9-bus test system with 3 generators.
    Commonly used for power systems textbook examples and stability studies.

    Returns:
        Network: 9 buses, 9 branches, 3 generators, base_mva=100.

    Example::

        import surge
        net = surge.case9()
        sol = surge.solve_ac_pf(net)
        print(sol.converged, sol.max_mismatch)
    """
    ...

def case14() -> Network:
    """IEEE 14-bus system.

    The IEEE 14-bus test system with 5 generators and 11 load buses.
    Frequently used as a small benchmark for power flow and OPF validation.

    Returns:
        Network: 14 buses, 20 branches, 5 generators, base_mva=100.
    """
    ...

def case30() -> Network:
    """IEEE 30-bus system.

    The IEEE 30-bus test system with 6 generators. Covers a wider range of
    voltage levels (132 kV and 33 kV) and includes transformer taps.

    Returns:
        Network: 30 buses, 41 branches, 6 generators, base_mva=100.
    """
    ...

def market30() -> Network:
    """Market-enabled IEEE 30-bus derivative.

    A custom variant of the IEEE 30-bus case carrying the reserve / offer
    schedule / dispatchable-load market primitives used by the dispatch
    and market crates.

    Returns:
        Network: 30 buses, 41 branches, 10 generators, base_mva=100.
    """
    ...

def case57() -> Network:
    """IEEE 57-bus system.

    The IEEE 57-bus test system with 7 generators. A medium-sized case
    often used to benchmark AC power flow convergence speed.

    Returns:
        Network: 57 buses, 80 branches, 7 generators, base_mva=100.
    """
    ...

def case118() -> Network:
    """IEEE 118-bus system.

    The IEEE 118-bus test system with 54 generators. One of the most widely
    cited power flow and OPF benchmarks in the literature.

    Returns:
        Network: 118 buses, 186 branches, 54 generators, base_mva=100.
    """
    ...

def case300() -> Network:
    """IEEE 300-bus system.

    The IEEE 300-bus test system with 69 generators. A larger benchmark
    for evaluating solver performance on multi-area networks.

    Returns:
        Network: 300 buses, 411 branches, 69 generators, base_mva=100.
    """
    ...

# ---------------------------------------------------------------------------
# Breaker adequacy (surge-fault/breaker_adequacy)
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Conditional limits (dispatch)
# ---------------------------------------------------------------------------

def apply_conditional_limits(network: Network, active_conditions: list[str]) -> None:
    """Apply conditional thermal limits to branches.

    Updates branch thermal ratings based on active conditions (e.g.
    contingency-specific ratings). Modifies the network in place.

    Args:
        network: Network to modify.
        active_conditions: List of condition IDs to activate.
    """
    ...

def reset_conditional_limits(network: Network) -> None:
    """Reset all branch thermal ratings to their original (base) values.

    Undoes any changes made by ``apply_conditional_limits()``.

    Args:
        network: Network to reset (modified in place).
    """
    ...

def get_conditional_limits(network: Network) -> dict[int, list[dict[str, Any]]]:
    """Get the conditional limits registered on a Network.

    Returns:
        Dict mapping branch index to a list of dicts, each with
        ``condition_id`` (str), ``rate_a`` (float), ``rate_c`` (float).
    """
    ...

# ---------------------------------------------------------------------------
# Rich element objects — additional types
# ---------------------------------------------------------------------------

class PumpedHydroUnit:
    """A pumped hydro storage unit. Obtain via ``net.pumped_hydro_units``."""

    def __init__(
        self, name: str, generator_bus: int, generator_id: str, capacity_mwh: float
    ) -> None: ...

    @property
    def name(self) -> str: ...
    @property
    def generator_bus(self) -> int: ...
    @property
    def generator_id(self) -> str: ...
    @property
    def variable_speed(self) -> bool: ...
    @property
    def pump_mw_fixed(self) -> float: ...
    @property
    def pump_mw_min(self) -> Optional[float]: ...
    @property
    def pump_mw_max(self) -> Optional[float]: ...
    @property
    def mode_transition_min(self) -> float: ...
    @property
    def condenser_capable(self) -> bool: ...
    @property
    def upper_reservoir_mwh(self) -> float: ...
    @property
    def lower_reservoir_mwh(self) -> float: ...
    @property
    def soc_initial_mwh(self) -> float: ...
    @property
    def soc_min_mwh(self) -> float: ...
    @property
    def soc_max_mwh(self) -> float: ...
    @property
    def efficiency_generate(self) -> float: ...
    @property
    def efficiency_pump(self) -> float: ...
    @property
    def n_units(self) -> int: ...
    @property
    def shared_penstock_mw_max(self) -> Optional[float]: ...
    @property
    def min_release_mw(self) -> float: ...
    @property
    def ramp_rate_mw_per_min(self) -> Optional[float]: ...
    @property
    def startup_time_gen_min(self) -> float: ...
    @property
    def startup_time_pump_min(self) -> float: ...
    @property
    def startup_cost(self) -> float: ...
    @property
    def reserve_offers(self) -> list[tuple[str, float, float]]: ...
    @property
    def qualifications(self) -> dict[str, bool]: ...

class BreakerRating:
    """Circuit breaker rating at a bus. Obtain via ``net.breaker_ratings``."""

    def __init__(
        self,
        bus: int,
        name: str,
        rated_kv: float,
        interrupting_ka: float,
        momentary_ka: Optional[float] = None,
        clearing_time_cycles: float = 5.0,
        in_service: bool = True,
    ) -> None: ...

    @property
    def bus(self) -> int: ...
    @property
    def name(self) -> str: ...
    @property
    def rated_kv(self) -> float: ...
    @property
    def interrupting_ka(self) -> float: ...
    @property
    def momentary_ka(self) -> Optional[float]: ...
    @property
    def clearing_time_cycles(self) -> float: ...
    @property
    def in_service(self) -> bool: ...

class FixedShunt:
    """A fixed shunt device at a bus. Obtain via ``net.fixed_shunts``."""

    def __init__(
        self,
        bus: int,
        id: str,
        shunt_type: str = "Capacitor",
        g_mw: float = 0.0,
        b_mvar: float = 0.0,
        in_service: bool = True,
        rated_kv: Optional[float] = None,
        rated_mvar: Optional[float] = None,
    ) -> None: ...

    @property
    def bus(self) -> int: ...
    @property
    def id(self) -> str: ...
    @property
    def shunt_type(self) -> str:
        """'Capacitor', 'Reactor', or 'HarmonicFilter'."""
        ...
    @property
    def g_mw(self) -> float: ...
    @property
    def b_mvar(self) -> float: ...
    @property
    def in_service(self) -> bool: ...
    @property
    def rated_kv(self) -> Optional[float]: ...
    @property
    def rated_mvar(self) -> Optional[float]: ...

class CombinedCycleConfig:
    """A single combined cycle configuration (e.g. '1x0', '2x1')."""

    def __init__(
        self,
        name: str,
        gen_indices: list[int],
        p_min_mw: float = 0.0,
        p_max_mw: float = 0.0,
        min_up_time_hr: float = 0.0,
        min_down_time_hr: float = 0.0,
    ) -> None: ...

    @property
    def name(self) -> str: ...
    @property
    def gen_indices(self) -> list[int]: ...
    @property
    def p_min_mw(self) -> float: ...
    @property
    def p_max_mw(self) -> float: ...
    @property
    def min_up_time_hr(self) -> float: ...
    @property
    def min_down_time_hr(self) -> float: ...

class CombinedCycleTransition:
    """A transition between two combined cycle configurations."""

    def __init__(
        self,
        from_config: str,
        to_config: str,
        transition_time_min: float = 0.0,
        transition_cost: float = 0.0,
        online_transition: bool = False,
    ) -> None: ...

    @property
    def from_config(self) -> str: ...
    @property
    def to_config(self) -> str: ...
    @property
    def transition_time_min(self) -> float: ...
    @property
    def transition_cost(self) -> float: ...
    @property
    def online_transition(self) -> bool: ...

class CombinedCyclePlant:
    """A combined cycle power plant. Obtain via ``net.combined_cycle_plants``."""

    def __init__(
        self,
        name: str,
        configs: Optional[list[CombinedCycleConfig]] = None,
        transitions: Optional[list[CombinedCycleTransition]] = None,
        active_config: Optional[str] = None,
        hours_in_config: float = 0.0,
        duct_firing_capable: bool = False,
    ) -> None: ...

    @property
    def name(self) -> str: ...
    @property
    def configs(self) -> list[CombinedCycleConfig]: ...
    @property
    def transitions(self) -> list[CombinedCycleTransition]: ...
    @property
    def active_config(self) -> Optional[str]: ...
    @property
    def hours_in_config(self) -> float: ...
    @property
    def duct_firing_capable(self) -> bool: ...

class OutageEntry:
    """An outage or derate event. Obtain via ``net.outage_entries``."""

    def __init__(
        self,
        category: str,
        start_hr: float,
        end_hr: float,
        outage_type: str = "Planned",
        derate_factor: float = 0.0,
        reason: Optional[str] = None,
        bus: Optional[int] = None,
        id: Optional[str] = None,
        from_bus: Optional[int] = None,
        to_bus: Optional[int] = None,
        circuit: Optional[str] = None,
        grid_id: Optional[int] = None,
        name: Optional[str] = None,
        terminal: Optional[str] = None,
    ) -> None: ...

    @property
    def schedule_index(self) -> Optional[int]: ...
    @property
    def category(self) -> str:
        """Equipment category: 'Generator', 'Branch', 'Load', etc."""
        ...
    @property
    def bus(self) -> Optional[int]: ...
    @bus.setter
    def bus(self, value: Optional[int]) -> None: ...
    @property
    def id(self) -> Optional[str]: ...
    @id.setter
    def id(self, value: Optional[str]) -> None: ...
    @property
    def from_bus(self) -> Optional[int]: ...
    @from_bus.setter
    def from_bus(self, value: Optional[int]) -> None: ...
    @property
    def to_bus(self) -> Optional[int]: ...
    @to_bus.setter
    def to_bus(self, value: Optional[int]) -> None: ...
    @property
    def circuit(self) -> Optional[str]: ...
    @circuit.setter
    def circuit(self, value: Optional[str]) -> None: ...
    @property
    def grid_id(self) -> Optional[int]: ...
    @grid_id.setter
    def grid_id(self, value: Optional[int]) -> None: ...
    @property
    def name(self) -> Optional[str]: ...
    @name.setter
    def name(self, value: Optional[str]) -> None: ...
    @property
    def terminal(self) -> Optional[str]: ...
    @terminal.setter
    def terminal(self, value: Optional[str]) -> None: ...
    @property
    def start_hr(self) -> float: ...
    @property
    def end_hr(self) -> float: ...
    @property
    def outage_type(self) -> str:
        """'Planned', 'Forced', 'Derate', or 'Mothballed'."""
        ...
    @property
    def derate_factor(self) -> float: ...
    @property
    def reason(self) -> Optional[str]: ...

class ReserveZone:
    """A reserve zone with zonal reserve requirements. Obtain via ``net.reserve_zones``."""

    def __init__(
        self,
        name: str,
        zonal_requirements: Optional[list[tuple[int, str, float]]] = None,
    ) -> None: ...

    @property
    def name(self) -> str: ...
    @property
    def zonal_requirements(self) -> list[tuple[int, str, float]]: ...
