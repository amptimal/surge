# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""
surge — AC/DC power-systems analysis for Python.

The public contract lives at the package layer. The native ``_surge`` module is
an implementation detail loaded from the installed package or a local source
tree build artifact.
"""

from __future__ import annotations

import importlib
import importlib.util
import os
import sys
from importlib.machinery import EXTENSION_SUFFIXES, ExtensionFileLoader
from pathlib import Path


# Names from the native module that are re-exported as ``surge.*``.
_NATIVE_PUBLIC_EXPORTS = (
    # Exceptions
    "SurgeError", "ConvergenceError", "InfeasibleError", "NetworkError",
    "UnsupportedFeatureError", "TopologyError", "MissingTopologyError", "StaleTopologyError",
    "AmbiguousTopologyError", "TopologyIntegrityError", "SurgeIOError",
    # Core model
    "Network", "Hvdc", "Bus", "Branch", "Generator", "StorageParams",
    "Load", "DispatchableLoad", "LccHvdcLink", "VscHvdcLink",
    "DcBus", "DcBranch", "DcConverter", "FactsDevice", "AreaSchedule",
    "BreakerRating", "FixedShunt", "ReserveZone", "PumpedHydroUnit",
    "CombinedCycleConfig", "CombinedCyclePlant", "CombinedCycleTransition",
    "OutageEntry",
    # Topology model
    "NodeBreakerTopology", "TopologyMapping", "TopologyBusSplit",
    "TopologyBusMerge", "CollapsedBranch", "TopologyReport",
    "TopologyRebuildResult", "Substation", "VoltageLevel", "Bay",
    "ConnectivityNode", "BusbarSection", "TerminalConnection", "TopologySwitch",
    # Results
    "AcPfResult", "DcPfResult", "OpfResult", "BindingContingency",
    "ContingencyViolation", "FailedContingencyEvaluation",
    "ScopfScreeningStats", "ContingencyAnalysis",
    "HvdcLccDetail", "HvdcStationSolution", "HvdcDcBusSolution", "HvdcResult",
    # Study / domain classes
    "Contingency", "ContingencyOptions",
    # Entry points re-exported at package level
    "version", "init_logging", "set_max_threads", "get_max_threads",
    "analyze_n1_branch", "analyze_n2_branch", "analyze_n1_generator",
    "analyze_contingencies", "solve_hvdc",
    "case9", "case14", "case30", "case57", "case118", "case300",
)

# All native exports the package depends on.  The source-tree bootstrap
# validates that the built artifact exposes every one of these before
# accepting it.  This is the **single source of truth** — every name used
# by a submodule via ``_native.<name>`` must appear here.
_REQUIRED_NATIVE_EXPORTS = frozenset(_NATIVE_PUBLIC_EXPORTS) | frozenset((
    # Solvers routed through the Python opf / powerflow layers
    "load", "save", "solve_ac_pf", "solve_dc_pf", "solve_fdpf",
    "solve_dc_opf_full", "solve_ac_opf", "solve_scopf",
    # DC sensitivity namespace
    "PreparedDcStudy", "PtdfResult", "LodfResult", "LodfMatrixResult",
    "N2LodfResult", "N2LodfBatchResult", "OtdfResult",
    "compute_ptdf", "prepare_dc_study", "compute_lodf",
    "compute_lodf_matrix", "compute_n2_lodf", "compute_n2_lodf_batch",
    "compute_otdf",
    # Contingency / stability namespace
    "ContingencyStudy", "CorrectiveAction", "PreparedCorrectiveDispatchStudy",
    "RemedialAction", "VoltageStressBus", "VoltageStressOptions", "VoltageStressResult",
    "analyze_branch_eens", "apply_ras", "compute_voltage_stress",
    "generate_breaker_contingencies", "n1_branch_study", "n1_generator_study",
    "n2_branch_study", "prepare_corrective_dispatch_study",
    "rank_contingencies", "solve_corrective_dispatch",
    # Transfer namespace
    "AcAtcResult", "AfcResult", "AtcOptions", "BldfResult", "Flowgate",
    "GsfResult", "InjectionCapabilityResult", "MultiTransferResult",
    "NercAtcResult", "TransferPath", "TransferStudy",
    "compute_ac_atc", "compute_afc", "compute_bldf", "compute_gsf",
    "compute_injection_capability", "compute_multi_transfer",
    "compute_nerc_atc", "prepare_transfer_study",
    # Powerflow helpers
    "PreparedAcPf", "JacobianResult", "YBusResult",
    # Batch / parameter sweep
    "SweepResult", "SweepResults", "parameter_sweep",
    # I/O helpers (prefixed with _ = package-private)
    "_CgmesProfiles", "_DynamicModel", "_LsfResult", "_SeqStats",
    "_compose_merge_networks", "_load_as", "_loads", "_loads_bytes",
    "_save_as", "_dumps", "_dumps_bytes",
    "_io_json_save", "_io_json_dumps",
    "_io_cgmes_save", "_io_cgmes_to_profiles",
    "_io_export_write_network_csv", "_io_export_write_solution_snapshot",
    "_io_geo_apply_bus_coordinates",
    "_io_profiles_read_load_csv", "_io_profiles_read_renewable_csv",
    "_io_psse_sequence_apply", "_io_psse_sequence_apply_text",
    "_io_psse_dyr_load", "_io_psse_dyr_loads",
    "_io_psse_dyr_save", "_io_psse_dyr_dumps",
    "_losses_compute_factors", "_units_ohm_to_pu",
))


def _is_source_tree_package() -> tuple[bool, Path | None]:
    package_dir = Path(__file__).resolve().parent
    repo_root = next(
        (
            parent
            for parent in package_dir.parents
            if (parent / "Cargo.toml").exists() and (parent / "src" / "surge-py").exists()
        ),
        None,
    )
    if repo_root is None:
        return False, None
    source_package_root = repo_root / "src" / "surge-py" / "python"
    try:
        package_dir.relative_to(source_package_root)
    except ValueError:
        return False, None
    return True, repo_root


def _source_tree_native_candidate() -> Path | None:
    is_source_tree, repo_root = _is_source_tree_package()
    if not is_source_tree or repo_root is None:
        return None

    target_root = Path(os.environ["CARGO_TARGET_DIR"]) if os.environ.get("CARGO_TARGET_DIR") else repo_root / "target"
    target_dir = target_root / "release"
    for candidate in (
        target_dir / "lib_surge.so",
        target_dir / "lib_surge.dylib",
        target_dir / "_surge.pyd",
    ):
        if candidate.exists():
            return candidate
    return None


def _packaged_native_candidate() -> Path | None:
    package_dir = Path(__file__).resolve().parent
    for suffix in EXTENSION_SUFFIXES:
        candidate = package_dir / f"_surge{suffix}"
        if candidate.exists():
            return candidate
    return None


def _packaged_copt_nlp_shim() -> Path | None:
    package_dir = Path(__file__).resolve().parent
    for name in ("libsurge_copt_nlp.so", "libsurge_copt_nlp.dylib", "surge_copt_nlp.dll"):
        candidate = package_dir / name
        if candidate.exists():
            return candidate
    return None


def _configure_packaged_copt_nlp_shim() -> None:
    if os.environ.get("SURGE_COPT_NLP_SHIM_PATH"):
        return

    shim = _packaged_copt_nlp_shim()
    if shim is not None:
        os.environ["SURGE_COPT_NLP_SHIM_PATH"] = os.fspath(shim)


def _native_module_name() -> str:
    return f"{__name__}._surge"


def _load_native_from_path(path: Path):
    module_name = _native_module_name()
    previous_module = sys.modules.pop(module_name, None)

    def restore_previous() -> None:
        if previous_module is None:
            sys.modules.pop(module_name, None)
        else:
            sys.modules[module_name] = previous_module

    loader = ExtensionFileLoader(module_name, str(path))
    spec = importlib.util.spec_from_file_location(module_name, str(path), loader=loader)
    if spec is None or spec.loader is None:
        restore_previous()
        raise ImportError(f"could not create module spec for {path}")
    try:
        module = importlib.util.module_from_spec(spec)
    except Exception as exc:
        restore_previous()
        raise ImportError(f"could not create native module object for {path}") from exc
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as exc:
        restore_previous()
        raise ImportError(f"could not load native module from {path}") from exc
    return module


def _import_native():
    is_source_tree, repo_root = _is_source_tree_package()
    candidate = _source_tree_native_candidate() if is_source_tree else _packaged_native_candidate()
    if candidate is None:
        location = (
            f"source-tree build artifact under {repo_root / 'target' / 'release'} "
            "(build with `maturin develop --release`)"
            if is_source_tree and repo_root is not None
            else f"packaged extension in {Path(__file__).resolve().parent}"
        )
        raise ImportError(f"could not find native surge extension at the expected {location}")

    module_name = _native_module_name()
    previous_module = sys.modules.get(module_name)
    module = _load_native_from_path(candidate)
    missing = sorted(name for name in _REQUIRED_NATIVE_EXPORTS if not hasattr(module, name))
    if missing:
        if previous_module is None:
            sys.modules.pop(module_name, None)
        else:
            sys.modules[module_name] = previous_module
        raise ImportError(
            "loaded native surge extension is missing required exports: "
            + ", ".join(missing)
        )
    return module


def _bind_native_public(module) -> None:
    for name in _NATIVE_PUBLIC_EXPORTS:
        globals()[name] = getattr(module, name)


_configure_packaged_copt_nlp_shim()
_native = _import_native()
_bind_native_public(_native)
_native_load = _native.load
_native_save = _native.save
from ._study_inputs import HvdcOpfLink, ParSetpoint, VirtualBid  # noqa: F401
from .opf import (  # noqa: F401
    AcAngleWarmStartMode,
    AcOpfOptions,
    AcOpfResult,
    AcOpfRuntime,
    ConstraintScreening,
    DcCostModel,
    DcLossModel,
    DcOpfOptions,
    DcOpfResult,
    DcOpfRuntime,
    DiscreteMode,
    GeneratorLimitMode,
    HvdcMode,
    ScopfFormulation,
    ScopfMode,
    ScopfOptions,
    ScopfResult,
    ScopfRuntime,
    ScopfScreeningPolicy,
    ThermalRating,
    solve_ac_opf,
    solve_dc_opf,
    solve_scopf,
)
from .powerflow import AcPfOptions, DcPfOptions, solve_ac_pf, solve_dc_pf  # noqa: F401


def load(path):
    """Load a network from a filesystem path."""

    return _native_load(os.fspath(path))


def save(network, path):
    """Save a network using extension-based format detection."""

    return _native_save(network, os.fspath(path))


_PYTHON_PUBLIC_EXPORTS = (
    "AcPfOptions",
    "DcPfOptions",
    "AcAngleWarmStartMode",
    "AcOpfOptions",
    "AcOpfResult",
    "AcOpfRuntime",
    "ConstraintScreening",
    "DcCostModel",
    "DcLossModel",
    "DcOpfOptions",
    "DcOpfResult",
    "DcOpfRuntime",
    "DiscreteMode",
    "GeneratorLimitMode",
    "HvdcMode",
    "HvdcOpfLink",
    "ScopfFormulation",
    "ScopfMode",
    "ScopfOptions",
    "ScopfResult",
    "ScopfRuntime",
    "ScopfScreeningPolicy",
    "ThermalRating",
    "ParSetpoint",
    "VirtualBid",
    "load",
    "save",
    "solve_ac_pf",
    "solve_dc_pf",
    "solve_ac_opf",
    "solve_dc_opf",
    "solve_scopf",
)

_LAZY_MODULES = {
    "audit": "surge.audit",
    "batch": "surge.batch",
    "compose": "surge.compose",
    "construction": "surge.construction",
    "contingency": "surge.contingency",
    "contingency_io": "surge.contingency_io",
    "dc": "surge.dc",
    "io": "surge.io",
    "losses": "surge.losses",
    "opf": "surge.opf",
    "powerflow": "surge.powerflow",
    "subsystem": "surge.subsystem",
    "transfer": "surge.transfer",
    "units": "surge.units",
}

__all__ = list(
    dict.fromkeys([*_NATIVE_PUBLIC_EXPORTS, *_PYTHON_PUBLIC_EXPORTS, *_LAZY_MODULES])
)


def __getattr__(name: str):
    module_name = _LAZY_MODULES.get(name)
    if module_name is not None:
        module = importlib.import_module(module_name)
        globals()[name] = module
        return module
    raise AttributeError(f"module 'surge' has no attribute {name!r}")


def __dir__() -> list[str]:
    return sorted(set(globals()) | set(_LAZY_MODULES))
