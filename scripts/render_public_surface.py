#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

"""Render generated public-surface docs for the CLI and Python package."""

from __future__ import annotations

import argparse
import ast
import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
DOCS_DIR = REPO_ROOT / "docs" / "generated"
PYTHON_PACKAGE_DIR = REPO_ROOT / "src" / "surge-py" / "python" / "surge"

PYTHON_MODULES = [
    ("surge", PYTHON_PACKAGE_DIR / "__init__.pyi"),
    ("surge.audit", PYTHON_PACKAGE_DIR / "audit.py"),
    ("surge.batch", PYTHON_PACKAGE_DIR / "batch.py"),
    ("surge.compose", PYTHON_PACKAGE_DIR / "compose.pyi"),
    ("surge.construction", PYTHON_PACKAGE_DIR / "construction.py"),
    ("surge.contingency", PYTHON_PACKAGE_DIR / "contingency.pyi"),
    ("surge.contingency_io", PYTHON_PACKAGE_DIR / "contingency_io.py"),
    ("surge.dc", PYTHON_PACKAGE_DIR / "dc.pyi"),
    ("surge.dispatch", PYTHON_PACKAGE_DIR / "dispatch.pyi"),
    ("surge.io", PYTHON_PACKAGE_DIR / "io" / "__init__.pyi"),
    ("surge.losses", PYTHON_PACKAGE_DIR / "losses.pyi"),
    ("surge.market", PYTHON_PACKAGE_DIR / "market" / "__init__.py"),
    ("surge.market.go_c3", PYTHON_PACKAGE_DIR / "market" / "go_c3.py"),
    ("surge.opf", PYTHON_PACKAGE_DIR / "opf.py"),
    ("surge.powerflow", PYTHON_PACKAGE_DIR / "powerflow.pyi"),
    ("surge.subsystem", PYTHON_PACKAGE_DIR / "subsystem.py"),
    ("surge.transfer", PYTHON_PACKAGE_DIR / "transfer.pyi"),
    ("surge.units", PYTHON_PACKAGE_DIR / "units.pyi"),
]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--cli-bin",
        type=Path,
        default=None,
        help="Path to a built surge-solve binary. Defaults to target/debug or cargo run fallback.",
    )
    parser.add_argument(
        "--docs-dir",
        type=Path,
        default=DOCS_DIR,
        help="Directory to write generated docs into.",
    )
    return parser.parse_args()


def run_cli_help(cli_bin: Path | None) -> str:
    candidate = cli_bin
    if candidate is None:
        default_bin = REPO_ROOT / "target" / "debug" / "surge-solve"
        if default_bin.exists():
            candidate = default_bin

    if candidate is not None:
        command = [str(candidate), "--help"]
    else:
        command = ["cargo", "run", "--quiet", "--bin", "surge-solve", "--", "--help"]

    completed = subprocess.run(
        command,
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return completed.stdout.strip()


def extract_possible_values(help_text: str, option_name: str) -> list[str]:
    lines = help_text.splitlines()
    for index, line in enumerate(lines):
        if option_name not in line:
            continue
        # Scan the option line itself and the next few lines for the values tag.
        for offset in range(index, min(index + 8, len(lines))):
            candidate = lines[offset]
            prefix = "[possible values: "
            pos = candidate.find(prefix)
            if pos == -1:
                continue
            tail = candidate[pos + len(prefix) :]
            bracket = tail.find("]")
            if bracket == -1:
                continue
            return [value.strip() for value in tail[:bracket].split(",")]
    return []


def format_annotation(node: ast.AST | None) -> str:
    if node is None:
        return ""
    return ast.unparse(node)


def format_default(node: ast.AST | None) -> str:
    if node is None:
        return ""
    return ast.unparse(node)


def format_arg(arg: ast.arg, default: ast.AST | None = None, prefix: str = "") -> str:
    rendered = prefix + arg.arg
    annotation = format_annotation(arg.annotation)
    if annotation:
        rendered += f": {annotation}"
    if default is not None:
        rendered += f" = {format_default(default)}"
    return rendered


def format_signature(node: ast.FunctionDef | ast.AsyncFunctionDef) -> str:
    args = node.args
    rendered: list[str] = []

    positional = list(args.posonlyargs) + list(args.args)
    defaults = [None] * (len(positional) - len(args.defaults)) + list(args.defaults)

    for index, arg in enumerate(args.posonlyargs):
        rendered.append(format_arg(arg, defaults[index]))
    if args.posonlyargs:
        rendered.append("/")

    offset = len(args.posonlyargs)
    for index, arg in enumerate(args.args):
        rendered.append(format_arg(arg, defaults[offset + index]))

    if args.vararg is not None:
        rendered.append(format_arg(args.vararg, prefix="*"))
    elif args.kwonlyargs:
        rendered.append("*")

    for arg, default in zip(args.kwonlyargs, args.kw_defaults):
        rendered.append(format_arg(arg, default))

    if args.kwarg is not None:
        rendered.append(format_arg(args.kwarg, prefix="**"))

    return_annotation = format_annotation(node.returns)
    rendered_signature = f"{node.name}({', '.join(rendered)})"
    if return_annotation:
        rendered_signature += f" -> {return_annotation}"
    return rendered_signature


def parse_module(path: Path) -> dict[str, list[str]]:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))

    functions: list[str] = []
    classes: list[str] = []
    reexports: list[str] = []
    namespaces: list[str] = []

    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and not node.name.startswith("_"):
            functions.append(format_signature(node))
            continue

        if isinstance(node, ast.ClassDef) and not node.name.startswith("_"):
            classes.append(node.name)
            continue

        if isinstance(node, ast.ImportFrom):
            names = [alias.asname or alias.name for alias in node.names]
            public_names = [name for name in names if not name.startswith("_")]
            if not public_names:
                continue
            if node.module is None:
                namespaces.extend(public_names)
                continue

            is_relative_import = node.level > 0
            is_surge_import = node.module.startswith("surge")
            if is_relative_import or is_surge_import:
                reexports.extend(public_names)

    return {
        "functions": functions,
        "classes": classes,
        "reexports": sorted(dict.fromkeys(reexports)),
        "namespaces": sorted(dict.fromkeys(namespaces)),
    }


def render_cli_surface(help_text: str) -> str:
    methods = extract_possible_values(help_text, "--method <METHOD>")
    solvers = extract_possible_values(help_text, "--solver <SOLVER>")
    lines = [
        "# Generated CLI Surface",
        "",
        "_Generated by `scripts/render_public_surface.py`. Do not edit directly._",
        "",
        "## Canonical Methods",
        "",
    ]
    if methods:
        lines.extend(f"- `{method}`" for method in methods)
    else:
        lines.append("- Could not parse method list from `surge-solve --help`.")

    lines.extend(["", "## Solver Backends", ""])
    if solvers:
        lines.extend(f"- `{solver}`" for solver in solvers)
    else:
        lines.append("- Could not parse solver backend list from `surge-solve --help`.")

    lines.extend(
        [
            "",
            "## Full Help Output",
            "",
            "```text",
            help_text,
            "```",
            "",
        ]
    )
    return "\n".join(lines)


def render_python_root(module_data: dict[str, list[str]]) -> str:
    lines = [
        "# Generated Python Root Surface",
        "",
        "_Generated by `scripts/render_public_surface.py` from `src/surge-py/python/surge/__init__.pyi`._",
        "",
        "## Root Functions",
        "",
    ]
    lines.extend(f"- `{signature}`" for signature in module_data["functions"])

    lines.extend(["", "## Public Namespaces", ""])
    lines.extend(f"- `{name}`" for name in module_data["namespaces"])

    lines.extend(["", "## Root Re-Exports", ""])
    lines.extend(f"- `{name}`" for name in module_data["reexports"])
    lines.append("")
    return "\n".join(lines)


def render_python_namespaces(entries: list[tuple[str, dict[str, list[str]]]]) -> str:
    lines = [
        "# Generated Python Namespace Surface",
        "",
        "_Generated by `scripts/render_public_surface.py`. Do not edit directly._",
        "",
    ]

    for module_name, module_data in entries:
        lines.extend([f"## `{module_name}`", ""])
        if module_data["functions"]:
            lines.append("### Functions")
            lines.append("")
            lines.extend(f"- `{signature}`" for signature in module_data["functions"])
            lines.append("")
        if module_data["classes"]:
            lines.append("### Classes")
            lines.append("")
            lines.extend(f"- `{name}`" for name in module_data["classes"])
            lines.append("")
        if module_data["reexports"]:
            lines.append("### Re-Exports")
            lines.append("")
            lines.extend(f"- `{name}`" for name in module_data["reexports"])
            lines.append("")
        if not module_data["functions"] and not module_data["classes"] and not module_data["reexports"]:
            lines.extend(["- No public top-level symbols found.", ""])

    return "\n".join(lines)


def write_file(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def main() -> int:
    args = parse_args()

    cli_help = run_cli_help(args.cli_bin)
    root_data = parse_module(PYTHON_PACKAGE_DIR / "__init__.pyi")
    namespace_entries = [
        (module_name, parse_module(path))
        for module_name, path in PYTHON_MODULES[1:]
    ]

    write_file(args.docs_dir / "cli-surface.md", render_cli_surface(cli_help))
    write_file(args.docs_dir / "python-root-surface.md", render_python_root(root_data))
    write_file(
        args.docs_dir / "python-namespace-surface.md",
        render_python_namespaces(namespace_entries),
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
