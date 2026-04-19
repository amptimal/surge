#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Generate ``surge/_generated/dispatch_request.py`` from the Rust JSON schema.

Walks the JSON schema emitted by ``cargo run -p surge-dispatch --bin
emit-schema`` and produces a Python ``TypedDict`` / ``Literal`` / ``Union``
surface that mirrors the Rust ``DispatchRequest`` tree exactly.

Usage::

    python3 scripts/codegen_dispatch_request.py
    # or, to regenerate from a custom schema file:
    python3 scripts/codegen_dispatch_request.py --schema /path/schema.json

Idempotent: running with no upstream changes produces no diff (CI
gate). Passes ``--check`` to fail with a non-zero exit code if the
generated file is stale.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import textwrap
from dataclasses import dataclass
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUT = REPO_ROOT / "src" / "surge-py" / "python" / "surge" / "_generated" / "dispatch_request.py"
DEFAULT_SCHEMA_CMD = ["cargo", "run", "-q", "-p", "surge-dispatch", "--bin", "emit-schema"]

HEADER = '''\
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
# ============================================================================
# AUTO-GENERATED — DO NOT EDIT.
# Regenerate with `python3 scripts/codegen_dispatch_request.py`.
# Source of truth: surge_dispatch::DispatchRequest (see
# `src/surge-dispatch/src/request.rs`). The Rust types derive
# ``schemars::JsonSchema``; this module mirrors that schema as Python
# ``TypedDict`` / ``Literal`` aliases for IDE + type-checker support.
# ============================================================================
"""Typed view of the ``DispatchRequest`` dict accepted by ``surge.solve_dispatch``.

Fields whose Rust type lives in another crate (``surge_network::market::*``,
``surge_opf::AcOpfOptions``, ``surge_solution::ParSetpoint``) are typed as
``dict[str, Any]`` / ``list[dict[str, Any]]`` because the schema treats
them as opaque JSON values. Build those payloads with the helpers in
``surge.market`` (``GeneratorOfferSchedule``, ``ReserveProductDef``,
``build_reserve_products_dict``, ...).
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict, Union


'''


@dataclass
class GenContext:
    defs: dict[str, dict[str, Any]]
    out_lines: list[str]
    emitted: set[str]
    pending: list[str]


# ---------------------------------------------------------------------------
# Schema-walking helpers
# ---------------------------------------------------------------------------


def _ref_name(node: dict[str, Any]) -> str | None:
    """If *node* is a ``$ref``, return the referenced definition name."""
    ref = node.get("$ref")
    if ref is None:
        return None
    if not ref.startswith("#/definitions/"):
        raise ValueError(f"unsupported $ref form: {ref!r}")
    return ref[len("#/definitions/"):]


def _unwrap_allof(node: dict[str, Any]) -> dict[str, Any]:
    """Schemars wraps ``$ref`` in ``allOf: [{$ref: ...}]`` when the field
    has a default; collapse that pattern to the bare ref."""
    all_of = node.get("allOf")
    if isinstance(all_of, list) and len(all_of) == 1 and "$ref" in all_of[0]:
        merged = dict(node)
        merged.pop("allOf")
        merged.update(all_of[0])
        return merged
    return node


def _is_string_literal_variant(variant: dict[str, Any]) -> str | None:
    """Return the string value if *variant* is a single-string enum
    (used by snake_case unit-variant Rust enums)."""
    if variant.get("type") != "string":
        return None
    enum = variant.get("enum")
    if not isinstance(enum, list) or len(enum) != 1:
        return None
    value = enum[0]
    if not isinstance(value, str):
        return None
    return value


def _is_null_variant(variant: dict[str, Any]) -> bool:
    return variant.get("type") == "null"


# ---------------------------------------------------------------------------
# Type reference rendering (right-hand side of TypedDict fields)
# ---------------------------------------------------------------------------


def render_type(node: Any, ctx: GenContext) -> str:
    """Render a JSON-schema node as a Python type expression."""
    if node is True or node == {}:
        return "Any"
    if node is False:
        # Schema that matches nothing — treat as Any for ergonomics.
        return "Any"
    if not isinstance(node, dict):
        return "Any"
    node = _unwrap_allof(node)

    ref = _ref_name(node)
    if ref is not None:
        # Forward references are fine — `from __future__ import annotations`
        # defers evaluation. Track for emission.
        if ref not in ctx.emitted and ref not in ctx.pending:
            ctx.pending.append(ref)
        return ref

    # ``oneOf`` — variants
    if "oneOf" in node:
        return _render_one_of(node["oneOf"], ctx)

    # ``anyOf`` — typically used by schemars for ``Option<T>``
    if "anyOf" in node:
        return _render_one_of(node["anyOf"], ctx)

    json_type = node.get("type")
    if isinstance(json_type, list):
        # e.g. ``["integer", "null"]`` for nullable scalars
        return _render_union_types(json_type, node, ctx)

    if json_type == "object":
        properties = node.get("properties")
        if properties is None:
            additional = node.get("additionalProperties")
            if additional is True or additional is None:
                return "dict[str, Any]"
            inner = render_type(additional, ctx)
            return f"dict[str, {inner}]"
        # Inline object — emit nested TypedDict-equivalent as dict[str, Any]
        # (rare; properties at the top level are handled per definition).
        return "dict[str, Any]"

    if json_type == "array":
        items = node.get("items", {})
        if isinstance(items, list):
            # Tuple of fixed length
            inner = ", ".join(render_type(item, ctx) for item in items)
            return f"tuple[{inner}]"
        return f"list[{render_type(items, ctx)}]"

    if json_type == "string":
        enum = node.get("enum")
        if isinstance(enum, list) and all(isinstance(v, str) for v in enum):
            return _render_literal(enum)
        return "str"

    if json_type == "integer":
        return "int"

    if json_type == "number":
        return "float"

    if json_type == "boolean":
        return "bool"

    if json_type == "null":
        return "None"

    if "const" in node:
        const = node["const"]
        if isinstance(const, str):
            return f'Literal[{const!r}]'
        return repr(const)

    # Fallback: anything goes
    return "Any"


def _render_union_types(types: list[str], node: dict[str, Any], ctx: GenContext) -> str:
    parts = []
    for t in types:
        synthetic = dict(node)
        synthetic["type"] = t
        synthetic.pop("enum", None) if t != "string" else None
        parts.append(render_type(synthetic, ctx))
    return _format_union(parts)


def _render_one_of(variants: list[dict[str, Any]], ctx: GenContext) -> str:
    # All string-only Literal variants → Literal[...]
    string_values = [_is_string_literal_variant(v) for v in variants]
    if all(v is not None for v in string_values):
        return _render_literal([v for v in string_values if v is not None])

    # Otherwise: union of variant types. Strip nulls to emit ``X | None``.
    rendered: list[str] = []
    has_null = False
    for variant in variants:
        if _is_null_variant(variant):
            has_null = True
            continue
        rendered.append(render_type(variant, ctx))
    if has_null:
        rendered.append("None")
    return _format_union(rendered)


def _format_union(parts: list[str]) -> str:
    parts = [p for p in parts if p]
    seen: list[str] = []
    for p in parts:
        if p not in seen:
            seen.append(p)
    if len(seen) == 1:
        return seen[0]
    return "Union[" + ", ".join(seen) + "]"


def _render_literal(values: list[str]) -> str:
    return "Literal[" + ", ".join(repr(v) for v in values) + "]"


# ---------------------------------------------------------------------------
# Definition rendering (TypedDict / alias / Union per definition)
# ---------------------------------------------------------------------------


def render_definition(name: str, node: dict[str, Any], ctx: GenContext) -> list[str]:
    description = (node.get("description") or "").strip()

    if "oneOf" in node:
        return _render_oneof_definition(name, node, description, ctx)

    if "anyOf" in node:
        # Treat anyOf at definition level as union alias
        rhs = _render_one_of(node["anyOf"], ctx)
        return _emit_alias(name, rhs, description)

    if node.get("type") == "string" and isinstance(node.get("enum"), list):
        rhs = _render_literal([v for v in node["enum"] if isinstance(v, str)])
        return _emit_alias(name, rhs, description)

    if node.get("type") == "object" or "properties" in node:
        return _render_typeddict_definition(name, node, description, ctx)

    # Scalar newtype (e.g. transparent struct AreaId(u32))
    rhs = render_type(node, ctx)
    return _emit_alias(name, rhs, description)


def _render_oneof_definition(
    name: str, node: dict[str, Any], description: str, ctx: GenContext
) -> list[str]:
    variants = node["oneOf"]

    # All string Literal variants → single Literal alias
    string_values = [_is_string_literal_variant(v) for v in variants]
    if all(v is not None for v in string_values):
        rhs = _render_literal([v for v in string_values if v is not None])
        return _emit_alias(name, rhs, description)

    # Tagged-object union (CommitmentPolicy):
    # Each non-string variant is an object with one required key.
    variant_types: list[str] = []
    inline_typeddicts: list[list[str]] = []
    sub_idx = 0
    for variant in variants:
        if _is_string_literal_variant(variant) is not None:
            variant_types.append(_render_literal([_is_string_literal_variant(variant)]))
            continue
        if variant.get("type") == "object":
            sub_name = f"{name}_Variant{sub_idx}"
            sub_idx += 1
            inline_typeddicts.append(
                _render_typeddict_definition(
                    sub_name, variant, (variant.get("description") or "").strip(), ctx
                )
            )
            variant_types.append(sub_name)
            continue
        variant_types.append(render_type(variant, ctx))

    lines: list[str] = []
    for sub in inline_typeddicts:
        lines.extend(sub)
    rhs = _format_union(variant_types)
    lines.extend(_emit_alias(name, rhs, description))
    return lines


def _render_typeddict_definition(
    name: str, node: dict[str, Any], description: str, ctx: GenContext
) -> list[str]:
    properties: dict[str, dict[str, Any]] = node.get("properties", {})
    required: set[str] = set(node.get("required", []))

    has_required = bool(required)
    has_optional = any(field not in required for field in properties)
    use_total_false = has_optional and not all(field in required for field in properties)

    lines: list[str] = []
    if has_required and has_optional:
        # Mixed: emit a base required class + the optional class extending it
        base_name = f"_{name}Required"
        lines.extend(_emit_typeddict_class(
            base_name,
            {k: v for k, v in properties.items() if k in required},
            total=True,
            description=None,
            ctx=ctx,
            base_classes="TypedDict",
        ))
        lines.append("")
        lines.extend(_emit_typeddict_class(
            name,
            {k: v for k, v in properties.items() if k not in required},
            total=False,
            description=description,
            ctx=ctx,
            base_classes=base_name,
        ))
    else:
        total = not use_total_false and not has_optional
        if not properties and not has_required:
            total = False
        lines.extend(_emit_typeddict_class(
            name,
            properties,
            total=total,
            description=description,
            ctx=ctx,
            base_classes="TypedDict",
        ))
    return lines


def _emit_typeddict_class(
    name: str,
    properties: dict[str, dict[str, Any]],
    *,
    total: bool,
    description: str | None,
    ctx: GenContext,
    base_classes: str,
) -> list[str]:
    if base_classes == "TypedDict":
        suffix = "" if total else ", total=False"
        header = f"class {name}({base_classes}{suffix}):"
    else:
        suffix = "" if total else ", total=False"
        header = f"class {name}({base_classes}{suffix}):"

    body: list[str] = []
    if description:
        body.append(_format_docstring(description))
    if not properties and not body:
        body.append("pass")
    for field_name, field_node in properties.items():
        ann = render_type(field_node, ctx)
        # Quote the annotation only when it contains characters that
        # would confuse a type-checker without future annotations.
        py_field = _safe_field(field_name)
        body.append(f"{py_field}: {ann}")

    return [header, *_indent_block(body)]


def _safe_field(field_name: str) -> str:
    # Python keyword guard
    keywords = {"from", "class", "def", "import", "lambda", "True", "False", "None"}
    if field_name in keywords:
        return f"{field_name}_"
    return field_name


def _emit_alias(name: str, rhs: str, description: str) -> list[str]:
    lines: list[str] = []
    if description:
        lines.append(f"# {description.splitlines()[0]}")
    lines.append(f"{name} = {rhs}")
    return lines


def _format_docstring(description: str) -> str:
    text = description.strip()
    if "\n" in text:
        wrapped = "\n".join(textwrap.wrap(text, width=78))
        return f'"""{wrapped}\n"""'
    return f'"""{text}"""'


def _indent_block(lines: list[str], indent: str = "    ") -> list[str]:
    return [(indent + line) if line else "" for line in lines]


# ---------------------------------------------------------------------------
# Top-level orchestration
# ---------------------------------------------------------------------------


def topological_order(defs: dict[str, dict[str, Any]]) -> list[str]:
    """Return a deterministic ordering: leaf types before composites.

    Pure topological sort by ``$ref`` graph; ties broken alphabetically.
    """
    deps: dict[str, set[str]] = {name: set() for name in defs}
    for name, node in defs.items():
        for ref in _walk_refs(node):
            if ref != name and ref in deps:
                deps[name].add(ref)

    order: list[str] = []
    visited: set[str] = set()
    visiting: set[str] = set()

    def visit(node: str) -> None:
        if node in visited:
            return
        if node in visiting:
            # Cycle — break by emitting the node now (forward refs handled
            # via `from __future__ import annotations`).
            return
        visiting.add(node)
        for dep in sorted(deps.get(node, ())):
            visit(dep)
        visiting.remove(node)
        visited.add(node)
        order.append(node)

    for name in sorted(defs):
        visit(name)
    return order


def _walk_refs(node: Any) -> list[str]:
    out: list[str] = []
    if isinstance(node, dict):
        ref = node.get("$ref")
        if isinstance(ref, str) and ref.startswith("#/definitions/"):
            out.append(ref[len("#/definitions/"):])
        for v in node.values():
            out.extend(_walk_refs(v))
    elif isinstance(node, list):
        for item in node:
            out.extend(_walk_refs(item))
    return out


def render_module(schema: dict[str, Any]) -> str:
    defs = dict(schema.get("definitions", {}))

    # Add the top-level DispatchRequest under its title.
    title = schema.get("title", "DispatchRequest")
    if title not in defs:
        # Strip the metadata that lives only on the root.
        root = {k: v for k, v in schema.items() if k not in ("$schema", "definitions")}
        defs[title] = root

    ctx = GenContext(defs=defs, out_lines=[], emitted=set(), pending=[])
    order = topological_order(defs)

    # Emit in topological order so the generated file reads naturally
    # from leaf scalars upward.
    chunks: list[str] = []
    for name in order:
        node = defs[name]
        block = render_definition(name, node, ctx)
        chunks.append("\n".join(block))
        ctx.emitted.add(name)

    body = "\n\n\n".join(chunks)

    # Emit __all__ for stable public surface.
    all_names = sorted(name for name in order if not name.startswith("_"))
    all_block = "__all__ = [\n" + "\n".join(
        f"    {n!r}," for n in all_names
    ) + "\n]\n"

    return HEADER + body + "\n\n\n" + all_block


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def load_schema(schema_path: Path | None) -> dict[str, Any]:
    if schema_path is not None:
        return json.loads(schema_path.read_text(encoding="utf-8"))
    output = subprocess.check_output(DEFAULT_SCHEMA_CMD, cwd=REPO_ROOT)
    return json.loads(output)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--schema",
        type=Path,
        default=None,
        help="path to a pre-emitted schema JSON (default: regenerate via cargo)",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=DEFAULT_OUT,
        help=f"output path (default: {DEFAULT_OUT})",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit 1 if the generated file would change (CI gate)",
    )
    args = parser.parse_args()

    schema = load_schema(args.schema)
    rendered = render_module(schema)

    args.out.parent.mkdir(parents=True, exist_ok=True)

    if args.check:
        if not args.out.exists():
            print(f"missing: {args.out}", file=sys.stderr)
            return 1
        existing = args.out.read_text(encoding="utf-8")
        if existing != rendered:
            print(
                f"stale: {args.out}\n"
                "  rerun: python3 scripts/codegen_dispatch_request.py",
                file=sys.stderr,
            )
            return 1
        print(f"ok: {args.out} is up to date")
        return 0

    args.out.write_text(rendered, encoding="utf-8")
    print(f"wrote: {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
