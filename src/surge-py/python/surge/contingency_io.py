# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Contingency definition file I/O — YAML, JSON, and PSS/E .con import."""

from __future__ import annotations

import json
import re
import warnings
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from ._surge import Contingency, Network

_SCHEMA_VERSION = 1


# ------------------------------------------------------------------
# Public helpers — generate standard contingency sets
# ------------------------------------------------------------------


def generate_n1_branch(network: Network) -> list[Contingency]:
    """Generate one N-1 contingency per in-service branch.

    Returns:
        List of ``Contingency`` objects, one per branch, ready to pass
        to ``analyze_contingencies()``.
    """
    from ._surge import Contingency as _Ctg

    from_buses = list(network.branch_from)
    to_buses = list(network.branch_to)
    circuits = list(network.branch_circuit)
    in_service = list(network.branch_in_service)
    ctgs: list[Contingency] = []
    for i in range(len(from_buses)):
        if not in_service[i]:
            continue
        f, t, c = from_buses[i], to_buses[i], circuits[i]
        ctgs.append(
            _Ctg(
                id=f"N-1_BR_{f}_{t}_{c}",
                branches=[(f, t, c)],
                label=f"Branch {f}-{t} ckt {c}",
            )
        )
    return ctgs


def generate_n1_generator(network: Network) -> list[Contingency]:
    """Generate one N-1 contingency per in-service generator.

    Returns:
        List of ``Contingency`` objects, one per generator.
    """
    from ._surge import Contingency as _Ctg

    gen_buses = list(network.gen_buses)
    gen_ids = list(network.gen_machine_id)
    gen_in_service = list(network.gen_in_service)
    ctgs: list[Contingency] = []
    for i in range(len(gen_buses)):
        if not gen_in_service[i]:
            continue
        bus, mid = gen_buses[i], gen_ids[i]
        ctgs.append(
            _Ctg(
                id=f"N-1_GEN_{bus}_{mid}",
                generators=[(bus, mid)],
                label=f"Gen at bus {bus} id {mid}",
            )
        )
    return ctgs


def generate_n1_all(network: Network) -> list[Contingency]:
    """Generate N-1 contingencies for all in-service branches and generators.

    Convenience function that combines :func:`generate_n1_branch` and
    :func:`generate_n1_generator`.

    Returns:
        List of ``Contingency`` objects (branches first, then generators).
    """
    return generate_n1_branch(network) + generate_n1_generator(network)


# ------------------------------------------------------------------
# Save
# ------------------------------------------------------------------


def _write_psse_con(contingencies: list[Contingency], path: Path) -> None:
    """Write contingencies in PSS/E .con format."""
    lines: list[str] = []
    for ctg in contingencies:
        lines.append(f"CONTINGENCY {ctg.id}")
        if ctg.branches:
            for br in ctg.branches:
                f, t = br[0], br[1]
                c = br[2] if len(br) > 2 else 1
                lines.append(f"  OPEN BRANCH FROM BUS {f} TO BUS {t} CIRCUIT {c}")
        if ctg.three_winding_transformers:
            for xfmr in ctg.three_winding_transformers:
                bi, bj, bk = xfmr[0], xfmr[1], xfmr[2]
                c = xfmr[3] if len(xfmr) > 3 else 1
                lines.append(
                    f"  OPEN THREE WINDING TRANSFORMER"
                    f" FROM BUS {bi} TO BUS {bj} TO BUS {bk} CIRCUIT {c}"
                )
        if ctg.generators:
            for gen in ctg.generators:
                bus, mid = gen[0], gen[1]
                lines.append(f"  REMOVE UNIT {mid} FROM BUS {bus}")
        lines.append("END")
    lines.append("")  # trailing newline
    path.write_text("\n".join(lines), encoding="utf-8")


def save_contingencies(
    contingencies: list[Contingency],
    path: str | Path,
    format: str = "yaml",
) -> None:
    """Save contingencies to a YAML, JSON, or PSS/E ``.con`` file.

    Args:
        contingencies: List of ``Contingency`` objects.
        path: Output file path.
        format: ``"yaml"`` (default), ``"json"``, or ``"con"`` (PSS/E).
    """
    path = Path(path)
    fmt = format.lower()

    if fmt == "con":
        _write_psse_con(contingencies, path)
        return

    records: list[dict[str, Any]] = []
    for ctg in contingencies:
        rec: dict[str, Any] = {"id": ctg.id}
        if ctg.label:
            rec["label"] = ctg.label
        if ctg.branches:
            rec["branches"] = [list(b) for b in ctg.branches]
        if ctg.generators:
            rec["generators"] = [list(g) for g in ctg.generators]
        if ctg.three_winding_transformers:
            rec["three_winding_transformers"] = [
                list(x) for x in ctg.three_winding_transformers
            ]
        records.append(rec)

    data = {"version": _SCHEMA_VERSION, "contingencies": records}

    if fmt == "json":
        path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    elif fmt in ("yaml", "yml"):
        try:
            import yaml
        except ImportError as exc:
            raise ImportError(
                "PyYAML is required for YAML output: pip install pyyaml"
            ) from exc
        path.write_text(yaml.safe_dump(data, sort_keys=False), encoding="utf-8")
    else:
        raise ValueError(f"Unsupported format {format!r} — use 'yaml', 'json', or 'con'")


# ------------------------------------------------------------------
# Load
# ------------------------------------------------------------------


def load_contingencies(
    path: str | Path,
    network: Network | None = None,
) -> list[Contingency]:
    """Load contingencies from a YAML, JSON, or PSS/E .con file.

    File format is auto-detected by extension:
    - ``.yaml``, ``.yml`` → YAML
    - ``.json`` → JSON
    - ``.con`` → PSS/E contingency format

    Args:
        path: Path to the contingency file.
        network: Optional network for PSS/E .con files. Required to expand
            ``SINGLE/DOUBLE ... IN SUBSYSTEM`` contingency specs. Ignored for
            YAML/JSON formats.

    Returns:
        List of ``Contingency`` objects.
    """
    path = Path(path)
    suffix = path.suffix.lower()

    if suffix in (".yaml", ".yml"):
        return _load_yaml(path)
    elif suffix == ".json":
        return _load_json(path)
    elif suffix == ".con":
        return import_psse_con(path, network=network)
    else:
        raise ValueError(
            f"Unknown contingency file extension {suffix!r} — "
            "expected .yaml, .yml, .json, or .con"
        )


def _load_yaml(path: Path) -> list[Contingency]:
    try:
        import yaml
    except ImportError as exc:
        raise ImportError(
            "PyYAML is required for YAML input: pip install pyyaml"
        ) from exc
    data = yaml.safe_load(path.read_text(encoding="utf-8"))
    return _parse_records(data)


def _load_json(path: Path) -> list[Contingency]:
    data = json.loads(path.read_text(encoding="utf-8"))
    return _parse_records(data)


def _parse_records(data: dict[str, Any]) -> list[Contingency]:
    from ._surge import Contingency as _Ctg

    ctgs: list[Contingency] = []
    for rec in data.get("contingencies", []):
        branches = [tuple(b) for b in rec.get("branches", [])] or None
        gens = [tuple(g) for g in rec.get("generators", [])] or None
        three_w = (
            [tuple(x) for x in rec.get("three_winding_transformers", [])] or None
        )
        ctgs.append(
            _Ctg(
                id=rec["id"],
                branches=branches,
                generators=gens,
                three_winding_transformers=three_w,
                label=rec.get("label"),
            )
        )
    return ctgs


# ------------------------------------------------------------------
# PSS/E .con importer — full command support
# ------------------------------------------------------------------

# PSS/E ACCC contingency file commands (from POM + PowerWorld docs):
#
# Element trip commands (fully supported):
#   CONTINGENCY <name>
#   END
#   OPEN BRANCH FROM BUS <i> TO BUS <j> [CIRCUIT|CKT <c>]
#   TRIP BRANCH FROM BUS <i> TO BUS <j> [CIRCUIT|CKT <c>]
#   DISCONNECT BRANCH FROM BUS <i> TO BUS <j> [CIRCUIT|CKT <c>]
#   SINGLE BRANCH FROM BUS <i> TO BUS <j> [CIRCUIT|CKT <c>]
#   OPEN THREE WINDING [TRANSFORMER] FROM BUS <i> TO BUS <j> TO BUS <k> [CKT <c>]
#   TRIP/DISCONNECT/REMOVE UNIT/MACHINE <id> FROM/AT BUS <n>
#   SET STATUS OPEN BRANCH FROM BUS <i> TO BUS <j> [CKT <c>]
#   SET STATUS OPEN UNIT/MACHINE <id> AT/FROM BUS <n>
#
# Simultaneous modifications (mapped to ContingencyModification):
#   CLOSE BRANCH FROM BUS <i> TO BUS <j> [CIRCUIT|CKT <c>]   → BranchClose
#   SET STATUS CLOSE BRANCH FROM BUS <i> TO BUS <j> [CKT <c>] → BranchClose
#   SET/CHANGE TAP [OF] BRANCH FROM BUS <i> TO BUS <j> [CKT <c>] TO <val> → BranchTap
#   SET/CHANGE PLOAD AT BUS <n> TO <val>                       → LoadSet
#   SET/CHANGE QLOAD AT BUS <n> TO <val>                       → LoadSet
#   SET/CHANGE LOAD AT BUS <n> TO <p> [<q>]                    → LoadSet
#   INCREASE/DECREASE PLOAD AT BUS <n> BY <val>                → LoadAdjust
#   INCREASE/DECREASE QLOAD AT BUS <n> BY <val>                → LoadAdjust
#   SET/CHANGE PGEN OF UNIT <id> AT BUS <n> TO <val>           → GenOutputSet
#   SET/CHANGE PMAX/PMIN OF UNIT <id> AT BUS <n> TO <val>      → GenLimitSet
#   INCREASE/DECREASE/CHANGE SHUNT AT BUS <n> BY <val>         → ShuntAdjust
#   CHANGE BUS TYPE BUS <n> TO TYPE <t>                        → BusTypeChange
#   CHANGE AREA INTERCHANGE <n> TO <val>                       → AreaScheduleSet
#
# Automatic subsystem contingency specs (expanded using network data):
#   SINGLE/DOUBLE BRANCH IN SUBSYSTEM <name>
#   SINGLE/DOUBLE UNIT/MACHINE IN SUBSYSTEM <name>
#   SINGLE/DOUBLE TIE IN SUBSYSTEM <name>
#
# DC line and switched-shunt contingencies (now modeled):
#   BLOCK TWOTERMDC '<name>'             → DcLineBlock modification
#   BLOCK VSCDC '<name>'                 → VscDcLineBlock modification
#   REMOVE SWSHUNT [<id>] FROM BUS <n>  → SwitchedShuntRemove modification

_RE_CTG_START = re.compile(r"^\s*CONTINGENCY\s+(.+)", re.IGNORECASE)
_RE_CTG_END = re.compile(r"^\s*END\b", re.IGNORECASE)

# Comment lines: COM ...
_RE_COMMENT = re.compile(r"^\s*COM\b", re.IGNORECASE)

# --- Branch outage commands (OPEN/TRIP/DISCONNECT/SINGLE — not CLOSE) ---
# SINGLE BRANCH FROM BUS ... is semantically identical to OPEN BRANCH FROM BUS ...
# CLOSE BRANCH is handled separately as a BranchClose modification.
_RE_BRANCH = re.compile(
    r"^\s*(?:OPEN|TRIP|DISCONNECT|SINGLE)\s+BRANCH\s+FROM\s+BUS\s+(\d+)"
    r"\s+TO\s+BUS\s+(\d+)"
    r"(?:\s+(?:CIRCUIT|CKT)\s+['\"]?(\S+?)['\"]?)?"
    r"(?:\s+WND\s+(\d+))?",
    re.IGNORECASE,
)

# --- CLOSE BRANCH → BranchClose modification ---
_RE_CLOSE_BRANCH = re.compile(
    r"^\s*CLOSE\s+BRANCH\s+FROM\s+BUS\s+(\d+)"
    r"\s+TO\s+BUS\s+(\d+)"
    r"(?:\s+(?:CIRCUIT|CKT)\s+['\"]?(\S+?)['\"]?)?",
    re.IGNORECASE,
)

# --- Three-winding transformer ---
# Matches both keyword form and bare positional form:
#   OPEN THREE WINDING [TRANSFORMER] FROM BUS <i> TO BUS <j> TO BUS <k> [CKT <c>]
#   OPEN THREE WINDING [TRANSFORMER] <i> <j> <k> [CKT <c>]
_RE_THREE_WINDING = re.compile(
    r"^\s*(?:OPEN|TRIP)\s+THREE\s+WINDING\s*(?:TRANSFORMER)?"
    r"\s+(?:FROM\s+BUS\s+)?(\d+)"
    r"\s+(?:TO\s+BUS\s+)?(\d+)"
    r"\s+(?:TO\s+BUS\s+)?(\d+)"
    r"(?:\s+(?:CIRCUIT|CKT)\s+['\"]?(\S+?)['\"]?)?",
    re.IGNORECASE,
)

# --- Generator/unit trip commands ---
# Matches: TRIP|DISCONNECT|REMOVE UNIT|MACHINE <id> AT|FROM BUS <n>
_RE_TRIP_UNIT = re.compile(
    r"^\s*(?:TRIP|DISCONNECT|REMOVE)\s+(?:UNIT|MACHINE)\s+['\"]?(\S+?)['\"]?"
    r"\s+(?:AT|FROM)\s+BUS\s+(\d+)",
    re.IGNORECASE,
)

# --- DC line block commands ---
# BLOCK TWOTERMDC '<name>'  → DcLineBlock modification
_RE_BLOCK_TWOTERMDC = re.compile(
    r"^\s*BLOCK\s+TWOTERMDC\s+['\"]?(.+?)['\"]?\s*$",
    re.IGNORECASE,
)
# BLOCK VSCDC '<name>'  → VscDcLineBlock modification
_RE_BLOCK_VSCDC = re.compile(
    r"^\s*BLOCK\s+VSCDC\s+['\"]?(.+?)['\"]?\s*$",
    re.IGNORECASE,
)

# --- Switched shunt remove command ---
# REMOVE SWSHUNT [<id>] FROM BUS <n>  → SwitchedShuntRemove modification
_RE_REMOVE_SWSHUNT = re.compile(
    r"^\s*REMOVE\s+SWSHUNT\b(?:\s+['\"]?\S+['\"]?)?\s+FROM\s+BUS\s+(\d+)",
    re.IGNORECASE,
)

# --- SET/CHANGE STATUS OPEN BRANCH → branch outage ---
# SET STATUS OPEN BRANCH FROM BUS <i> TO BUS <j> [CKT <c>]
_RE_SET_BRANCH_OPEN = re.compile(
    r"^\s*(?:SET|CHANGE|ALTER|MODIFY)\s+STATUS\s+OPEN\s+BRANCH"
    r"\s+FROM\s+BUS\s+(\d+)\s+TO\s+BUS\s+(\d+)"
    r"(?:\s+(?:CIRCUIT|CKT)\s+['\"]?(\S+?)['\"]?)?",
    re.IGNORECASE,
)

# --- SET/CHANGE STATUS OPEN UNIT/MACHINE → generator outage ---
_RE_SET_UNIT_OPEN = re.compile(
    r"^\s*(?:SET|CHANGE|ALTER|MODIFY)\s+STATUS\s+OPEN\s+(?:UNIT|MACHINE)\s+['\"]?(\S+?)['\"]?"
    r"\s+(?:AT|FROM)\s+BUS\s+(\d+)",
    re.IGNORECASE,
)

# --- SET/CHANGE STATUS CLOSE BRANCH → BranchClose modification ---
_RE_SET_BRANCH_CLOSE = re.compile(
    r"^\s*(?:SET|CHANGE|ALTER|MODIFY)\s+STATUS\s+(?:CLOSE|CLOSED)\s+BRANCH"
    r"\s+FROM\s+BUS\s+(\d+)\s+TO\s+BUS\s+(\d+)"
    r"(?:\s+(?:CIRCUIT|CKT)\s+['\"]?(\S+?)['\"]?)?",
    re.IGNORECASE,
)

# --- SET/CHANGE TAP OF BRANCH → BranchTap modification ---
# SET TAP [OF|ON] [BRANCH] FROM BUS <i> TO BUS <j> [CKT <c>] TO <val>
_RE_SET_TAP = re.compile(
    r"^\s*(?:SET|CHANGE)\s+TAP\s+(?:OF\s+|ON\s+)?(?:BRANCH\s+)?"
    r"FROM\s+BUS\s+(\d+)\s+TO\s+BUS\s+(\d+)"
    r"(?:\s+(?:CIRCUIT|CKT)\s+['\"]?(\S+?)['\"]?)?"
    r"\s+TO\s+([\d.Ee+\-]+)",
    re.IGNORECASE,
)

# --- SET/CHANGE PLOAD/QLOAD AT BUS → LoadSet modification ---
_RE_SET_PQ_LOAD = re.compile(
    r"^\s*(?:SET|CHANGE)\s+(PLOAD|QLOAD)\s+(?:AT|OF)\s+BUS\s+(\d+)"
    r"\s+TO\s+([\d.Ee+\-]+)",
    re.IGNORECASE,
)

# --- SET/CHANGE LOAD AT BUS (P and optional Q) → LoadSet modification ---
_RE_SET_LOAD = re.compile(
    r"^\s*(?:SET|CHANGE)\s+LOAD\s+(?:AT|OF|ON)\s+BUS\s+(\d+)"
    r"\s+TO\s+([\d.Ee+\-]+)(?:\s+([\d.Ee+\-]+))?",
    re.IGNORECASE,
)

# --- INCREASE/DECREASE PLOAD → LoadAdjust modification (P delta) ---
_RE_ADJUST_PLOAD = re.compile(
    r"^\s*(INCREASE|RAISE|DECREASE|REDUCE)\s+PLOAD\s+(?:AT|OF)\s+BUS\s+(\d+)"
    r"\s+BY\s+([\d.Ee+\-]+)",
    re.IGNORECASE,
)

# --- INCREASE/DECREASE QLOAD → LoadAdjust modification (Q delta) ---
_RE_ADJUST_QLOAD = re.compile(
    r"^\s*(INCREASE|RAISE|DECREASE|REDUCE)\s+QLOAD\s+(?:AT|OF)\s+BUS\s+(\d+)"
    r"\s+BY\s+([\d.Ee+\-]+)",
    re.IGNORECASE,
)

# --- SET/CHANGE PGEN → GenOutputSet modification ---
_RE_SET_PGEN = re.compile(
    r"^\s*(?:SET|CHANGE)\s+PGEN\s+(?:OF|FOR)\s+(?:UNIT|MACHINE)\s+['\"]?(\S+?)['\"]?"
    r"\s+(?:AT|FROM)\s+BUS\s+(\d+)\s+TO\s+([\d.Ee+\-]+)",
    re.IGNORECASE,
)

# --- SET/CHANGE PMAX/PMIN → GenLimitSet modification ---
_RE_SET_PLIM = re.compile(
    r"^\s*(?:SET|CHANGE)\s+(PMAX|PMIN)\s+(?:OF|FOR)\s+(?:UNIT|MACHINE)\s+['\"]?(\S+?)['\"]?"
    r"\s+(?:AT|FROM)\s+BUS\s+(\d+)\s+TO\s+([\d.Ee+\-]+)",
    re.IGNORECASE,
)

# --- INCREASE/DECREASE/CHANGE SHUNT → ShuntAdjust modification ---
_RE_ADJUST_SHUNT = re.compile(
    r"^\s*(INCREASE|RAISE|DECREASE|REDUCE|CHANGE)\s+(?:FIXED\s+)?SHUNT\s+(?:AT|OF)\s+BUS\s+(\d+)"
    r"\s+BY\s+([\d.Ee+\-]+)",
    re.IGNORECASE,
)

# --- CHANGE BUS TYPE → BusTypeChange modification ---
_RE_CHANGE_BUS_TYPE = re.compile(
    r"^\s*CHANGE\s+BUS\s+TYPE\s+(?:OF\s+|AT\s+)?BUS\s+(\d+)\s+TO\s+(?:TYPE\s+)?(\d+)",
    re.IGNORECASE,
)

# --- CHANGE AREA INTERCHANGE → AreaScheduleSet modification ---
_RE_CHANGE_AREA_INTERCHANGE = re.compile(
    r"^\s*CHANGE\s+AREA\s+(?:INTERCHANGE\s+|INTCHANGE\s+)(\d+)\s+TO\s+([\d.Ee+\-]+)",
    re.IGNORECASE,
)

# --- SUBSYSTEM definition block (appears outside CONTINGENCY blocks) ---
# SUBSYSTEM <name> / BUSES <n1> <n2> ... / END
_RE_SUBSYSTEM_DEF = re.compile(r"^\s*SUBSYSTEM\s+(\S+)", re.IGNORECASE)
_RE_SUBSYSTEM_BUSES = re.compile(r"^\s*BUSES\b(.*)", re.IGNORECASE)

# --- IN SUBSYSTEM spec — captures cardinality, element type, and subsystem name ---
# NOTE: requires "IN SUBSYSTEM" so SINGLE BRANCH FROM BUS ... falls through to _RE_BRANCH.
_RE_AUTO_SUBSYSTEM = re.compile(
    r"^\s*(SINGLE|DOUBLE)\s+(BRANCH|UNIT|MACHINE|TIE)\s+IN\s+SUBSYSTEM\s+(\S+)",
    re.IGNORECASE,
)

# --- Catchall for unrecognized SET/CHANGE/INCREASE/DECREASE commands ---
_RE_MODIFICATION_CATCHALL = re.compile(
    r"^\s*(?:SET|CHANGE|ALTER|MODIFY|INCREASE|RAISE|DECREASE|REDUCE)\s+",
    re.IGNORECASE,
)


def _parse_subsystems(lines: list[str]) -> dict[str, set[int]]:
    """Pre-parse ``SUBSYSTEM`` definition blocks from a PSS/E .con file.

    ``SUBSYSTEM`` blocks appear outside ``CONTINGENCY`` blocks and define named
    sets of buses used in ``SINGLE/DOUBLE ... IN SUBSYSTEM`` contingency specs.

    Args:
        lines: All lines of the .con file.

    Returns:
        Dict mapping subsystem name (uppercase) to set of bus numbers.
    """
    subsystems: dict[str, set[int]] = {}
    current_ss: str | None = None
    in_contingency = False

    for line in lines:
        stripped = line.strip()
        if not stripped or stripped.startswith("/") or stripped.startswith("@"):
            continue

        # Track contingency blocks so subsystem-looking lines inside them are skipped
        if _RE_CTG_START.match(stripped):
            in_contingency = True
            current_ss = None
            continue
        if _RE_CTG_END.match(stripped):
            if in_contingency:
                in_contingency = False
            elif current_ss is not None:
                current_ss = None  # END terminates the SUBSYSTEM block
            continue
        if in_contingency:
            continue

        # SUBSYSTEM <name> starts a new block
        m_ss = _RE_SUBSYSTEM_DEF.match(stripped)
        if m_ss:
            current_ss = m_ss.group(1).upper()
            subsystems[current_ss] = set()
            continue

        # BUSES line inside a SUBSYSTEM block
        if current_ss is not None:
            m_buses = _RE_SUBSYSTEM_BUSES.match(stripped)
            if m_buses:
                bus_nums = [int(x) for x in m_buses.group(1).split() if x.isdigit()]
                subsystems[current_ss].update(bus_nums)

    return subsystems


def _expand_subsystem_ctgs(
    base_name: str,
    base_branches: list[tuple[int, int, str]],
    base_generators: list[tuple[int, str]],
    base_three_winding: list[tuple[int, int, int, str]],
    base_modifications: list[dict[str, Any]],
    expansion_specs: list[tuple[str, str, str]],
    subsystem_defs: dict[str, set[int]],
    network: Network | None,
    ctgs: list[Contingency],
    _Ctg: type,
    unsupported_model: list[str],
) -> None:
    """Expand ``SINGLE/DOUBLE ... IN SUBSYSTEM`` specs into individual contingencies.

    For each spec, finds matching branches or generators from the network and
    creates one contingency per element (``SINGLE``) or per unordered pair
    (``DOUBLE``).  Branch type ``BRANCH`` matches internal branches (both
    endpoints in subsystem); ``TIE`` matches tie lines (exactly one endpoint
    in subsystem).  ``UNIT``/``MACHINE`` matches generators connected to a
    subsystem bus.

    If *network* is ``None``, a warning entry is added to *unsupported_model*
    and the spec is skipped.
    """
    if network is None:
        unsupported_model.append(
            f"  [{base_name}]: IN SUBSYSTEM expansion requires network= argument "
            f"to import_psse_con (skipped)"
        )
        return

    try:
        net_from = list(network.branch_from)
        net_to = list(network.branch_to)
        net_ckt = list(network.branch_circuit)
        net_br_active = list(network.branch_in_service)
        net_gen_buses = list(network.gen_buses)
        net_gen_ids = list(network.gen_machine_id)
        net_gen_active = list(network.gen_in_service)
    except AttributeError as exc:
        unsupported_model.append(
            f"  [{base_name}]: IN SUBSYSTEM expansion: network missing required "
            f"attribute ({exc}) — skipped"
        )
        return

    for cardinality, element_type, ss_name in expansion_specs:
        ss_buses = subsystem_defs.get(ss_name)
        if ss_buses is None:
            warnings.warn(
                f"import_psse_con: subsystem '{ss_name}' referenced in contingency "
                f"'{base_name}' was not defined in the .con file — skipped",
                stacklevel=4,
            )
            continue

        if element_type == "BRANCH":
            # Internal branches: both endpoints in subsystem
            elements = [
                (net_from[i], net_to[i], net_ckt[i])
                for i in range(len(net_from))
                if net_br_active[i]
                and net_from[i] in ss_buses
                and net_to[i] in ss_buses
            ]
            _emit_branch_ctgs(
                base_name, base_branches, base_generators, base_three_winding,
                base_modifications, elements, cardinality, ctgs, _Ctg,
            )

        elif element_type == "TIE":
            # Tie lines: exactly one endpoint in subsystem
            elements = [
                (net_from[i], net_to[i], net_ckt[i])
                for i in range(len(net_from))
                if net_br_active[i]
                and (net_from[i] in ss_buses) != (net_to[i] in ss_buses)
            ]
            _emit_branch_ctgs(
                base_name, base_branches, base_generators, base_three_winding,
                base_modifications, elements, cardinality, ctgs, _Ctg,
            )

        elif element_type in ("UNIT", "MACHINE"):
            # Generators connected to a subsystem bus
            gen_elements = [
                (net_gen_buses[i], net_gen_ids[i])
                for i in range(len(net_gen_buses))
                if net_gen_active[i] and net_gen_buses[i] in ss_buses
            ]
            if cardinality == "SINGLE":
                for gen in gen_elements:
                    ctg_id = f"{base_name}_{gen[0]}_{gen[1]}"
                    all_gens = base_generators + [gen]
                    ctgs.append(
                        _Ctg(
                            id=ctg_id,
                            branches=base_branches or None,
                            generators=all_gens or None,
                            three_winding_transformers=base_three_winding or None,
                            modifications=base_modifications or None,
                            label=ctg_id,
                        )
                    )
            else:  # DOUBLE
                for i, g1 in enumerate(gen_elements):
                    for g2 in gen_elements[i + 1 :]:
                        ctg_id = f"{base_name}_{g1[0]}_{g1[1]}_{g2[0]}_{g2[1]}"
                        all_gens = base_generators + [g1, g2]
                        ctgs.append(
                            _Ctg(
                                id=ctg_id,
                                branches=base_branches or None,
                                generators=all_gens or None,
                                three_winding_transformers=base_three_winding or None,
                                modifications=base_modifications or None,
                                label=ctg_id,
                            )
                        )


def _emit_branch_ctgs(
    base_name: str,
    base_branches: list[tuple[int, int, str]],
    base_generators: list[tuple[int, str]],
    base_three_winding: list[tuple[int, int, int, str]],
    base_modifications: list[dict[str, Any]],
    branch_elements: list[tuple[int, int, str]],
    cardinality: str,
    ctgs: list[Contingency],
    _Ctg: type,
) -> None:
    """Emit branch-based contingencies from a subsystem expansion."""
    if cardinality == "SINGLE":
        for br in branch_elements:
            ctg_id = f"{base_name}_{br[0]}_{br[1]}_{br[2]}"
            all_branches = base_branches + [br]
            ctgs.append(
                _Ctg(
                    id=ctg_id,
                    branches=all_branches or None,
                    generators=base_generators or None,
                    three_winding_transformers=base_three_winding or None,
                    modifications=base_modifications or None,
                    label=ctg_id,
                )
            )
    else:  # DOUBLE
        for i, br1 in enumerate(branch_elements):
            for br2 in branch_elements[i + 1 :]:
                ctg_id = f"{base_name}_{br1[0]}_{br1[1]}_{br2[0]}_{br2[1]}"
                all_branches = base_branches + [br1, br2]
                ctgs.append(
                    _Ctg(
                        id=ctg_id,
                        branches=all_branches or None,
                        generators=base_generators or None,
                        three_winding_transformers=base_three_winding or None,
                        modifications=base_modifications or None,
                        label=ctg_id,
                    )
                )


def import_psse_con(
    path: str | Path,
    network: Network | None = None,
) -> list[Contingency]:
    """Import a PSS/E ``.con`` contingency definition file.

    Fully supports:

    **Element outage commands** (branch/generator trips):
    - ``OPEN/TRIP/DISCONNECT/SINGLE BRANCH`` (two-winding)
    - ``OPEN THREE WINDING TRANSFORMER`` (three-winding)
    - ``TRIP/DISCONNECT/REMOVE UNIT/MACHINE`` (generators)
    - ``SET/CHANGE STATUS OPEN BRANCH`` or ``UNIT``

    **Simultaneous modifications** (mapped to :class:`~surge.Contingency`
    ``modifications`` list as ``ContingencyModification`` dicts):
    - ``CLOSE BRANCH`` / ``SET STATUS CLOSE BRANCH`` → ``BranchClose``
    - ``SET/CHANGE TAP [OF] BRANCH … TO <val>`` → ``BranchTap``
    - ``SET/CHANGE PLOAD/QLOAD AT BUS <n> TO <val>`` → ``LoadSet``
    - ``SET/CHANGE LOAD AT BUS <n> TO <p> [<q>]`` → ``LoadSet``
    - ``INCREASE/DECREASE PLOAD/QLOAD AT BUS <n> BY <val>`` → ``LoadAdjust``
    - ``SET/CHANGE PGEN OF UNIT <id> AT BUS <n> TO <val>`` → ``GenOutputSet``
    - ``SET/CHANGE PMAX/PMIN OF UNIT <id> AT BUS <n> TO <val>`` → ``GenLimitSet``
    - ``INCREASE/DECREASE/CHANGE SHUNT AT BUS <n> BY <val>`` → ``ShuntAdjust``
    - ``CHANGE BUS TYPE BUS <n> TO TYPE <t>`` → ``BusTypeChange``
    - ``CHANGE AREA INTERCHANGE <n> TO <val>`` → ``AreaScheduleSet``

    **Automatic subsystem contingency specs** (requires *network*):
    - ``SINGLE/DOUBLE BRANCH IN SUBSYSTEM <name>`` — one contingency per
      internal branch (or per pair for DOUBLE)
    - ``SINGLE/DOUBLE TIE IN SUBSYSTEM <name>`` — tie-line variant
    - ``SINGLE/DOUBLE UNIT/MACHINE IN SUBSYSTEM <name>`` — generator variant

    DC line and switched-shunt contingencies (modeled):
    - ``BLOCK TWOTERMDC '<name>'`` → :class:`DcLineBlock` modification
    - ``BLOCK VSCDC '<name>'`` → :class:`VscDcLineBlock` modification
    - ``REMOVE SWSHUNT [<id>] FROM BUS <n>`` → :class:`SwitchedShuntRemove` modification

    Args:
        path: Path to the PSS/E ``.con`` file.
        network: Network loaded from the companion ``.raw`` file. Required to
            expand ``IN SUBSYSTEM`` contingency specs; ignored otherwise.

    Returns:
        List of ``Contingency`` objects.
    """
    from ._surge import Contingency as _Ctg

    path = Path(path)
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()

    # Pre-pass: collect SUBSYSTEM block definitions
    subsystem_defs = _parse_subsystems(lines)

    ctgs: list[Contingency] = []
    current_name: str | None = None
    branches: list[tuple[int, int, int]] = []
    generators: list[tuple[int, str]] = []
    three_winding: list[tuple[int, int, int, int]] = []
    modifications: list[dict[str, Any]] = []
    # (cardinality, element_type, subsystem_name) tuples from IN SUBSYSTEM specs
    expansion_specs: list[tuple[str, str, str]] = []

    # Warning accumulators
    unsupported_model: list[str] = []         # IN SUBSYSTEM expansion errors (network=None)
    unrecognized_modification: list[str] = [] # SET/CHANGE forms not matched above
    unrecognized: list[str] = []              # completely unrecognized lines

    for lineno, line in enumerate(lines, 1):
        stripped = line.strip()
        # Skip empty lines, PSS/E header (/PSS...), @ directives, COM comments
        if (
            not stripped
            or stripped.startswith("/")
            or stripped.startswith("@")
            or _RE_COMMENT.match(stripped)
        ):
            continue

        # Skip SUBSYSTEM definition blocks (already pre-parsed above)
        if current_name is None and _RE_SUBSYSTEM_DEF.match(stripped):
            continue
        if current_name is None and _RE_SUBSYSTEM_BUSES.match(stripped):
            continue

        # --- CONTINGENCY start ---
        m_start = _RE_CTG_START.match(stripped)
        if m_start:
            current_name = m_start.group(1).strip().strip("'\"")
            branches = []
            generators = []
            three_winding = []
            modifications = []
            expansion_specs = []
            continue

        # --- Outside a CONTINGENCY block: skip ---
        if current_name is None:
            continue

        # --- END of current contingency ---
        m_end = _RE_CTG_END.match(stripped)
        if m_end:
            if expansion_specs:
                _expand_subsystem_ctgs(
                    current_name,
                    branches,
                    generators,
                    three_winding,
                    modifications,
                    expansion_specs,
                    subsystem_defs,
                    network,
                    ctgs,
                    _Ctg,
                    unsupported_model,
                )
            elif branches or generators or three_winding or modifications:
                ctgs.append(
                    _Ctg(
                        id=current_name,
                        branches=branches or None,
                        generators=generators or None,
                        three_winding_transformers=three_winding or None,
                        modifications=modifications or None,
                        label=current_name,
                    )
                )
            current_name = None
            continue

        # --- Three-winding transformer (must check before branch) ---
        m_3w = _RE_THREE_WINDING.match(stripped)
        if m_3w:
            bi = int(m_3w.group(1))
            bj = int(m_3w.group(2))
            bk = int(m_3w.group(3))
            ckt_str = m_3w.group(4)
            c = _parse_circuit(ckt_str)
            three_winding.append((bi, bj, bk, c))
            continue

        # --- Branch outage (OPEN/TRIP/DISCONNECT/SINGLE) ---
        m_br = _RE_BRANCH.match(stripped)
        if m_br:
            f, t = int(m_br.group(1)), int(m_br.group(2))
            c = _parse_circuit(m_br.group(3))
            branches.append((f, t, c))
            continue

        # --- CLOSE BRANCH → BranchClose modification ---
        m_close = _RE_CLOSE_BRANCH.match(stripped)
        if m_close:
            f, t = int(m_close.group(1)), int(m_close.group(2))
            c = _parse_circuit(m_close.group(3))
            modifications.append(
                {"type": "BranchClose", "from_bus": f, "to_bus": t, "circuit": c}
            )
            continue

        # --- Generator trip ---
        m_gen = _RE_TRIP_UNIT.match(stripped)
        if m_gen:
            mid = m_gen.group(1).strip("'\"")
            bus = int(m_gen.group(2))
            generators.append((bus, mid))
            continue

        # --- SET STATUS OPEN BRANCH → branch outage ---
        m_sbo = _RE_SET_BRANCH_OPEN.match(stripped)
        if m_sbo:
            f, t = int(m_sbo.group(1)), int(m_sbo.group(2))
            c = _parse_circuit(m_sbo.group(3))
            branches.append((f, t, c))
            continue

        # --- SET STATUS OPEN UNIT/MACHINE → generator outage ---
        m_suo = _RE_SET_UNIT_OPEN.match(stripped)
        if m_suo:
            mid = m_suo.group(1).strip("'\"")
            bus = int(m_suo.group(2))
            generators.append((bus, mid))
            continue

        # --- SET STATUS CLOSE BRANCH → BranchClose modification ---
        m_sbc = _RE_SET_BRANCH_CLOSE.match(stripped)
        if m_sbc:
            f, t = int(m_sbc.group(1)), int(m_sbc.group(2))
            c = _parse_circuit(m_sbc.group(3))
            modifications.append(
                {"type": "BranchClose", "from_bus": f, "to_bus": t, "circuit": c}
            )
            continue

        # --- SET/CHANGE TAP → BranchTap modification ---
        m_tap = _RE_SET_TAP.match(stripped)
        if m_tap:
            f, t = int(m_tap.group(1)), int(m_tap.group(2))
            c = _parse_circuit(m_tap.group(3))
            tap = float(m_tap.group(4))
            modifications.append(
                {"type": "BranchTap", "from_bus": f, "to_bus": t, "circuit": c, "tap": tap}
            )
            continue

        # --- SET/CHANGE PLOAD/QLOAD → LoadSet modification ---
        m_spq = _RE_SET_PQ_LOAD.match(stripped)
        if m_spq:
            pq_type = m_spq.group(1).upper()
            bus = int(m_spq.group(2))
            val = float(m_spq.group(3))
            if pq_type == "PLOAD":
                modifications.append(
                    {"type": "LoadSet", "bus": bus, "p_mw": val, "q_mvar": 0.0}
                )
            else:
                modifications.append(
                    {"type": "LoadSet", "bus": bus, "p_mw": 0.0, "q_mvar": val}
                )
            continue

        # --- SET/CHANGE LOAD AT BUS → LoadSet modification ---
        m_sl = _RE_SET_LOAD.match(stripped)
        if m_sl:
            bus = int(m_sl.group(1))
            p_mw = float(m_sl.group(2))
            q_mvar = float(m_sl.group(3)) if m_sl.group(3) else 0.0
            modifications.append(
                {"type": "LoadSet", "bus": bus, "p_mw": p_mw, "q_mvar": q_mvar}
            )
            continue

        # --- INCREASE/DECREASE PLOAD → LoadAdjust modification ---
        m_ap = _RE_ADJUST_PLOAD.match(stripped)
        if m_ap:
            verb = m_ap.group(1).upper()
            bus = int(m_ap.group(2))
            val = float(m_ap.group(3))
            sign = -1.0 if verb in ("DECREASE", "REDUCE") else 1.0
            modifications.append(
                {"type": "LoadAdjust", "bus": bus, "delta_p_mw": sign * val, "delta_q_mvar": 0.0}
            )
            continue

        # --- INCREASE/DECREASE QLOAD → LoadAdjust modification ---
        m_aq = _RE_ADJUST_QLOAD.match(stripped)
        if m_aq:
            verb = m_aq.group(1).upper()
            bus = int(m_aq.group(2))
            val = float(m_aq.group(3))
            sign = -1.0 if verb in ("DECREASE", "REDUCE") else 1.0
            modifications.append(
                {"type": "LoadAdjust", "bus": bus, "delta_p_mw": 0.0, "delta_q_mvar": sign * val}
            )
            continue

        # --- SET/CHANGE PGEN → GenOutputSet modification ---
        m_pg = _RE_SET_PGEN.match(stripped)
        if m_pg:
            mid = m_pg.group(1).strip("'\"")
            bus = int(m_pg.group(2))
            p_mw = float(m_pg.group(3))
            modifications.append(
                {"type": "GenOutputSet", "bus": bus, "machine_id": mid, "p_mw": p_mw}
            )
            continue

        # --- SET/CHANGE PMAX/PMIN → GenLimitSet modification ---
        m_pl = _RE_SET_PLIM.match(stripped)
        if m_pl:
            limit_type = m_pl.group(1).upper()
            mid = m_pl.group(2).strip("'\"")
            bus = int(m_pl.group(3))
            val = float(m_pl.group(4))
            if limit_type == "PMAX":
                modifications.append(
                    {"type": "GenLimitSet", "bus": bus, "machine_id": mid,
                     "pmax_mw": val, "pmin_mw": None}
                )
            else:
                modifications.append(
                    {"type": "GenLimitSet", "bus": bus, "machine_id": mid,
                     "pmax_mw": None, "pmin_mw": val}
                )
            continue

        # --- INCREASE/DECREASE/CHANGE SHUNT → ShuntAdjust modification ---
        m_sh = _RE_ADJUST_SHUNT.match(stripped)
        if m_sh:
            verb = m_sh.group(1).upper()
            bus = int(m_sh.group(2))
            val = float(m_sh.group(3))
            sign = -1.0 if verb in ("DECREASE", "REDUCE") else 1.0
            modifications.append(
                {"type": "ShuntAdjust", "bus": bus, "delta_b_pu": sign * val}
            )
            continue

        # --- CHANGE BUS TYPE → BusTypeChange modification ---
        m_bt = _RE_CHANGE_BUS_TYPE.match(stripped)
        if m_bt:
            bus = int(m_bt.group(1))
            bus_type = int(m_bt.group(2))
            modifications.append({"type": "BusTypeChange", "bus": bus, "bus_type": bus_type})
            continue

        # --- CHANGE AREA INTERCHANGE → AreaScheduleSet modification ---
        m_ai = _RE_CHANGE_AREA_INTERCHANGE.match(stripped)
        if m_ai:
            area = int(m_ai.group(1))
            p_mw = float(m_ai.group(2))
            modifications.append({"type": "AreaScheduleSet", "area": area, "p_mw": p_mw})
            continue

        # --- SINGLE/DOUBLE ... IN SUBSYSTEM → defer expansion to END ---
        m_sub = _RE_AUTO_SUBSYSTEM.match(stripped)
        if m_sub:
            expansion_specs.append(
                (m_sub.group(1).upper(), m_sub.group(2).upper(), m_sub.group(3).upper())
            )
            continue

        # --- BLOCK TWOTERMDC → DcLineBlock modification ---
        m_dc2 = _RE_BLOCK_TWOTERMDC.match(stripped)
        if m_dc2:
            modifications.append({"type": "DcLineBlock", "name": m_dc2.group(1).strip()})
            continue

        # --- BLOCK VSCDC → VscDcLineBlock modification ---
        m_vsc = _RE_BLOCK_VSCDC.match(stripped)
        if m_vsc:
            modifications.append({"type": "VscDcLineBlock", "name": m_vsc.group(1).strip()})
            continue

        # --- REMOVE SWSHUNT → SwitchedShuntRemove modification ---
        m_ss = _RE_REMOVE_SWSHUNT.match(stripped)
        if m_ss:
            modifications.append({"type": "SwitchedShuntRemove", "bus": int(m_ss.group(1))})
            continue

        # --- Unrecognized SET/CHANGE/ALTER/MODIFY form ---
        if _RE_MODIFICATION_CATCHALL.match(stripped):
            unrecognized_modification.append(f"  line {lineno} [{current_name}]: {stripped}")
            continue

        # --- Truly unrecognized line ---
        unrecognized.append(f"  line {lineno} [{current_name}]: {stripped}")

    # Emit warnings
    if unsupported_model:
        parts = [
            f"import_psse_con: {len(unsupported_model)} IN SUBSYSTEM expansion(s) "
            f"in {path.name} skipped (pass network= to import_psse_con to enable):"
        ]
        parts.extend(unsupported_model[:5])
        if len(unsupported_model) > 5:
            parts.append(f"  ... and {len(unsupported_model) - 5} more")
        warnings.warn("\n".join(parts), stacklevel=2)

    if unrecognized_modification:
        parts = [
            f"import_psse_con: {len(unrecognized_modification)} unrecognized "
            f"SET/CHANGE/MODIFY command form(s) in {path.name} (skipped):"
        ]
        parts.extend(unrecognized_modification[:5])
        if len(unrecognized_modification) > 5:
            parts.append(f"  ... and {len(unrecognized_modification) - 5} more")
        warnings.warn("\n".join(parts), stacklevel=2)

    if unrecognized:
        parts = [
            f"import_psse_con: {len(unrecognized)} unrecognized command(s) "
            f"in {path.name}:"
        ]
        parts.extend(unrecognized[:5])
        if len(unrecognized) > 5:
            parts.append(f"  ... and {len(unrecognized) - 5} more")
        warnings.warn("\n".join(parts), stacklevel=2)

    return ctgs


def _parse_circuit(ckt_str: str | None) -> str:
    """Parse a PSS/E circuit identifier string to its canonical string form.

    Strips surrounding quotes and whitespace.  Returns the raw circuit
    identifier as a string (e.g. ``"1"``, ``"A"``), matching the string stored
    in ``Branch.circuit`` by the PSS/E RAW parser.

      - None or empty → ``"1"`` (PSS/E default circuit).
      - Otherwise → stripped, unquoted string (preserving original case).
    """
    if ckt_str is None:
        return "1"
    cleaned = ckt_str.strip("'\"").strip()
    if not cleaned:
        return "1"
    return cleaned
