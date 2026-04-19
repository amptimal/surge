#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Build a native Surge replay bundle from a GO C3 scenario.

A bundle is the directory layout that :mod:`dashboards.rto.api` and the
checked-in ``examples/cases/`` fixtures consume. Each bundle packages
the four canonical zst artifacts (network, dispatch request, GO C3
problem snapshot, adapter context) plus ``metadata.json`` / ``README.md``
/ ``PROVENANCE.md``.

Library entry points:

* :func:`build_native_bundle` — build one bundle from a
  :class:`NativeCaseDefinition` (resolves scenario from the manifest).
* :func:`build_bundle_from_scenario` — lower-level variant that takes an
  explicit scenario path.

CLI::

    uv run python -m benchmarks.go_c3.bundle --name go_c3_event4_73_d1_315_sw0
    uv run python -m benchmarks.go_c3.bundle --all  # rebuild every tracked case
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import zstandard as zstd

from .datasets import ScenarioRecord, discover_scenarios, ensure_dataset_unpacked
from .manifests import (
    DatasetManifest,
    NativeCaseDefinition,
    NativeCaseManifest,
    load_dataset_manifest,
    load_native_case_manifest,
)
from .paths import default_cache_root, repo_root


# Frozen policy written into metadata.json. Matches the shape the existing
# bundles ship (see d431b594). These are the `surge.MarketPolicy` fields
# the Rust `parse_policy` understands plus the Python-side solver knobs.
DEFAULT_FROZEN_POLICY: dict[str, Any] = {
    "ac_dispatch_fail_closed": True,
    "ac_nlp_solver": None,
    "ac_opf_overrides": None,
    "ac_reconcile_mode": "ac_dispatch",
    "ac_target_tracking_lmp_marginal_cost_config": None,
    "ac_target_tracking_overrides": None,
    "ac_target_tracking_reduced_cost_config": None,
    "allow_branch_switching": False,
    "capture_solver_log": False,
    "commitment_mip_rel_gap": None,
    "commitment_mode": "optimize",
    "commitment_seed_mode": "none",
    "commitment_solution_path": None,
    "commitment_time_limit_secs": None,
    "coupling": "time_coupled",
    "decommit_pmin_tolerance_mw": 1.0,
    "fixed_commitment_from_initial_status": False,
    "fixed_consumer_mode": "dispatchable",
    "fixed_hvdc_solution_path": None,
    "formulation": "dc",
    "include_security_screening": False,
    "log_level": "info",
    "lp_solver": "gurobi",
    "max_ac_refinement_iterations": 0,
    "max_decommit_iterations": 2,
    "run_pricing": False,
    "sced_ac_benders_overrides": None,
    "security_mode": "explicit_dc_ctg",
    "switchable_branch_uids": None,
    "use_sced_ac_benders": False,
}


BUS_LABEL = {
    "event4_73": "73-bus",
    "event4_617": "617-bus",
    "event4_1576": "1576-bus",
    "event4_2000": "2000-bus",
    "event4_4224": "4224-bus",
    "event4_6049": "6049-bus",
    "event4_6717": "6717-bus",
    "event4_8316_d1": "8316-bus",
    "event4_8316_d2": "8316-bus",
    "event4_8316_d3": "8316-bus",
    "event4_23643": "23643-bus",
}


README_TEMPLATE = """# {bundle}

This bundle packages a GO C3-derived native Surge replay fixture.

## Files

- `{bundle}.surge.json.zst` - canonical Surge network artifact
- `{bundle}.dispatch-request.json.zst` - canonical dispatch request artifact
- `{bundle}.goc3-problem.json.zst` - GO C3 problem snapshot used to build the native artifacts
- `{bundle}.goc3-context.json.zst` - adapter context required to export native results back to GO C3 solution format
- `metadata.json` - source scenario, derivation, and policy metadata
- `expected-validator-summary.json` - optional validator baseline for replay-verified bundles
- `PROVENANCE.md` - upstream source and refresh notes
"""


PROVENANCE_TEMPLATE = """# {bundle} Provenance

This bundle packages first-party Surge artifacts derived from a public GO Competition Challenge 3 dataset.

## Upstream Reference

- Dataset key: `{dataset}`
- Title: Challenge 3 Event 4 {bus_label} Synthetic Dataset Scenarios
- URL: `{dataset_url}`
- Released: {released_on}
- Scenario: `{division} {network_model} scenario_{scenario_id}`
- Switching mode: `{switching_mode}`

## Native Packaging Contract

- Canonical network artifact: `{bundle}.surge.json.zst`
- Canonical request artifact: `{bundle}.dispatch-request.json.zst`
- Canonical GO C3 problem snapshot: `{bundle}.goc3-problem.json.zst`
- Canonical adapter context snapshot: `{bundle}.goc3-context.json.zst`

## Refresh Procedure

1. Resolve the source GO C3 scenario from the native-case manifest.
2. Rebuild the native network, request, problem snapshot, and adapter context artifacts.
3. For interval-0 variants, slice the source problem to interval `0` before building native artifacts.
4. Re-run validator parity for checked-in replay baselines before updating expected summaries.
"""


# ---------------------------------------------------------------------------
# zstd helpers
# ---------------------------------------------------------------------------


def _write_zst(path: Path, data: bytes, *, level: int = 10) -> None:
    tmp = path.with_suffix(path.suffix + ".tmp")
    cctx = zstd.ZstdCompressor(level=level)
    with open(tmp, "wb") as fh:
        fh.write(cctx.compress(data))
    tmp.replace(path)


def _write_json_zst(path: Path, obj: Any) -> None:
    payload = json.dumps(obj, sort_keys=True, separators=(",", ":")).encode("utf-8")
    _write_zst(path, payload)


# ---------------------------------------------------------------------------
# Metadata / docs
# ---------------------------------------------------------------------------


def _interval_durations(problem_doc: dict[str, Any]) -> list[float]:
    general = problem_doc.get("time_series_input", {}).get("general", {}) or {}
    raw = general.get("interval_duration") or []
    return [float(x) for x in raw]


def _default_description(definition: NativeCaseDefinition) -> str:
    return definition.description


def _make_metadata(
    definition: NativeCaseDefinition,
    scenario_path: Path,
    problem_doc: dict[str, Any],
    request: dict[str, Any],
    *,
    bundle_name: str,
    variant: str = "full",
) -> dict[str, Any]:
    net = problem_doc["network"]
    ts = problem_doc.get("time_series_input", {})
    T = len(ts.get("simple_dispatchable_device", [{}])[0].get("p_ub", []))
    durations = _interval_durations(problem_doc)
    uniform = bool(durations) and all(abs(d - durations[0]) < 1e-12 for d in durations)

    counts = {
        "ac_line": len(net.get("ac_line", [])),
        "active_zonal_reserve": len(net.get("active_zonal_reserve", [])),
        "bus": len(net.get("bus", [])),
        "contingency": len(problem_doc.get("reliability", {}).get("contingency", [])),
        "dc_line": len(net.get("dc_line", [])),
        "reactive_zonal_reserve": len(net.get("reactive_zonal_reserve", [])),
        "shunt": len(net.get("shunt", [])),
        "simple_dispatchable_device": len(net.get("simple_dispatchable_device", [])),
        "two_winding_transformer": len(net.get("two_winding_transformer", [])),
    }

    rel_scenario = (
        str(scenario_path.relative_to(repo_root()))
        if scenario_path.is_absolute() and scenario_path.is_relative_to(repo_root())
        else str(scenario_path)
    )

    return {
        "artifacts": {
            "context": f"{bundle_name}.goc3-context.json.zst",
            "expected_validator_summary": "expected-validator-summary.json",
            "network": f"{bundle_name}.surge.json.zst",
            "problem": f"{bundle_name}.goc3-problem.json.zst",
            "request": f"{bundle_name}.dispatch-request.json.zst",
        },
        "bundle_name": bundle_name,
        "definition_name": definition.name,
        "derivation": {"kind": "full_horizon", "source_periods": T},
        "description": definition.description,
        "policy": DEFAULT_FROZEN_POLICY,
        "problem_summary": {
            "base_norm_mva": float(net.get("general", {}).get("base_norm_mva") or 100.0),
            "counts": counts,
            "interval_durations": durations,
            "periods": T,
            "problem_path": rel_scenario,
            "uniform_intervals": uniform,
        },
        "request_summary": {
            "has_runtime": "runtime" in request,
            "timeline_periods": int((request.get("timeline") or {}).get("periods") or T),
        },
        "source": {
            "dataset": definition.dataset,
            "division": definition.division,
            "network_model": definition.network_model,
            "notes": definition.notes or "",
            "scenario_id": definition.scenario_id,
            "source_problem_path": rel_scenario,
            "switching_mode": definition.switching_mode,
            "tier": definition.tier,
            "track_examples": definition.track_examples,
        },
        "variant": variant,
    }


def _write_docs(
    out_dir: Path,
    definition: NativeCaseDefinition,
    bundle_name: str,
    dataset_manifest: DatasetManifest,
) -> None:
    dataset = dataset_manifest.by_key().get(definition.dataset)
    dataset_url = dataset.url if dataset else ""
    released_on = dataset.released_on if dataset else "unknown"
    bus_label = BUS_LABEL.get(definition.dataset, definition.dataset)

    (out_dir / "PROVENANCE.md").write_text(
        PROVENANCE_TEMPLATE.format(
            bundle=bundle_name,
            dataset=definition.dataset,
            bus_label=bus_label,
            dataset_url=dataset_url,
            released_on=released_on,
            division=definition.division,
            network_model=definition.network_model,
            scenario_id=f"{definition.scenario_id:03d}",
            switching_mode=definition.switching_mode,
        ),
        encoding="utf-8",
    )
    (out_dir / "README.md").write_text(
        README_TEMPLATE.format(bundle=bundle_name), encoding="utf-8"
    )


# ---------------------------------------------------------------------------
# Core builder
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class BundleBuildReport:
    bundle_name: str
    out_dir: Path
    artifact_sizes: dict[str, int]
    t_build_network_secs: float
    t_build_request_secs: float


def _frozen_policy_for_market_api():
    """Construct the surge.market.go_c3.MarketPolicy that matches the frozen policy."""
    import surge.market.go_c3 as go_c3  # type: ignore

    return go_c3.MarketPolicy(
        formulation=DEFAULT_FROZEN_POLICY["formulation"],
        ac_reconcile_mode=DEFAULT_FROZEN_POLICY["ac_reconcile_mode"],
        consumer_mode=DEFAULT_FROZEN_POLICY["fixed_consumer_mode"],
        commitment_mode=DEFAULT_FROZEN_POLICY["commitment_mode"],
        allow_branch_switching=DEFAULT_FROZEN_POLICY["allow_branch_switching"],
        lp_solver=DEFAULT_FROZEN_POLICY["lp_solver"],
    )


def build_bundle_from_scenario(
    definition: NativeCaseDefinition,
    scenario_path: Path,
    out_dir: Path,
    *,
    dataset_manifest: DatasetManifest | None = None,
    bundle_name: str | None = None,
) -> BundleBuildReport:
    """Build a full-horizon bundle from an explicit scenario JSON path.

    The ``definition`` carries the source metadata (dataset, division,
    scenario id, tier, etc.) baked into ``metadata.json``. The actual
    scenario content is read from ``scenario_path`` — this keeps the
    function usable both from the manifest-driven path and from ad hoc
    scenarios (e.g. a sliced t00 variant built in a temp dir).
    """
    import surge.io.json as surge_json  # type: ignore
    import surge.market.go_c3 as go_c3  # type: ignore

    bundle_name = bundle_name or definition.name
    out_dir.mkdir(parents=True, exist_ok=True)
    dataset_manifest = dataset_manifest or load_dataset_manifest()

    problem = go_c3.load(scenario_path)
    policy = _frozen_policy_for_market_api()

    t0 = time.perf_counter()
    network, context = go_c3.build_network(problem, policy)
    t_build_net = time.perf_counter() - t0

    t0 = time.perf_counter()
    request = go_c3.build_request(problem, policy)
    t_build_req = time.perf_counter() - t0

    net_path = out_dir / f"{bundle_name}.surge.json.zst"
    ctx_path = out_dir / f"{bundle_name}.goc3-context.json.zst"
    req_path = out_dir / f"{bundle_name}.dispatch-request.json.zst"
    prb_path = out_dir / f"{bundle_name}.goc3-problem.json.zst"

    surge_json.save(network, str(net_path))
    _write_json_zst(ctx_path, context)
    _write_json_zst(req_path, request)

    problem_doc = json.loads(scenario_path.read_text(encoding="utf-8"))
    _write_json_zst(prb_path, problem_doc)

    metadata = _make_metadata(
        definition,
        scenario_path,
        problem_doc,
        request,
        bundle_name=bundle_name,
    )
    (out_dir / "metadata.json").write_text(
        json.dumps(metadata, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    _write_docs(out_dir, definition, bundle_name, dataset_manifest)

    sizes = {p.name: p.stat().st_size for p in (net_path, ctx_path, req_path, prb_path)}
    return BundleBuildReport(
        bundle_name=bundle_name,
        out_dir=out_dir,
        artifact_sizes=sizes,
        t_build_network_secs=t_build_net,
        t_build_request_secs=t_build_req,
    )


def _locate_scenario_for_definition(
    definition: NativeCaseDefinition,
    dataset_manifest: DatasetManifest,
    cache_root: Path,
) -> ScenarioRecord:
    resource = dataset_manifest.by_key().get(definition.dataset)
    if resource is None:
        raise KeyError(
            f"definition {definition.name!r} references unknown dataset "
            f"{definition.dataset!r} (not in public-datasets.json)"
        )
    unpacked = ensure_dataset_unpacked(resource, cache_root=cache_root)
    for scenario in discover_scenarios(unpacked.unpacked_root, resource.key):
        if (
            scenario.division == definition.division
            and scenario.scenario_id == definition.scenario_id
        ):
            return scenario
    raise FileNotFoundError(
        f"scenario not found: dataset={definition.dataset} "
        f"division={definition.division} id={definition.scenario_id}"
    )


def build_native_bundle(
    definition: NativeCaseDefinition,
    *,
    examples_root: Path | None = None,
    cache_root: Path | None = None,
    dataset_manifest: DatasetManifest | None = None,
    variant: str = "full",
) -> BundleBuildReport:
    """Build a bundle described by a :class:`NativeCaseDefinition`.

    Resolves the source scenario from the dataset manifest + cache
    (unpacking the dataset archive on demand), then delegates to
    :func:`build_bundle_from_scenario`.

    Only the ``full`` variant is implemented today. ``t00`` slicing is
    flagged as a follow-up.
    """
    if variant != "full":
        raise NotImplementedError(
            f"variant {variant!r} not implemented — only 'full' is supported"
        )
    examples_root = examples_root or (repo_root() / "examples" / "cases")
    cache_root = cache_root or default_cache_root()
    dataset_manifest = dataset_manifest or load_dataset_manifest()

    scenario = _locate_scenario_for_definition(definition, dataset_manifest, cache_root)
    out_dir = examples_root / definition.name
    return build_bundle_from_scenario(
        definition,
        scenario.problem_path,
        out_dir,
        dataset_manifest=dataset_manifest,
        bundle_name=definition.name,
    )


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--name", action="append", help="Definition name to build. Repeat for multiple.")
    group.add_argument("--all", action="store_true", help="Build every case with track_examples=true.")
    parser.add_argument("--tier", help="Restrict --all to a tier (e.g. core, medium).")
    parser.add_argument("--examples-root", type=Path, help="Destination root (default: examples/cases/).")
    parser.add_argument("--cache-root", type=Path, help="Override GO C3 cache root.")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)
    manifest: NativeCaseManifest = load_native_case_manifest()
    dataset_manifest = load_dataset_manifest()

    if args.all:
        cases = [c for c in manifest.cases if c.track_examples]
        if args.tier:
            cases = [c for c in cases if c.tier == args.tier]
    else:
        by_name = manifest.by_name()
        cases = []
        for name in args.name:
            if name not in by_name:
                raise SystemExit(f"no native case manifest entry for {name!r}")
            cases.append(by_name[name])

    for definition in cases:
        print(f">>> building {definition.name}", flush=True)
        report = build_native_bundle(
            definition,
            examples_root=args.examples_root,
            cache_root=args.cache_root,
            dataset_manifest=dataset_manifest,
            variant="full",
        )
        print(f"  build_network={report.t_build_network_secs:.3f}s "
              f"build_request={report.t_build_request_secs:.3f}s", flush=True)
        for name, size in report.artifact_sizes.items():
            print(f"    {name}: {size:,} bytes", flush=True)
        print(f"  wrote {report.out_dir}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
