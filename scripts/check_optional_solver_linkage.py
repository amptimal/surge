#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Fail if a built artifact hard-links optional solver libraries."""

from __future__ import annotations

import argparse
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import zipfile


FORBIDDEN_PATTERNS = (
    re.compile(r"^libgurobi"),
    re.compile(r"^libcplex"),
    re.compile(r"^libipopt"),
    re.compile(r"^libcopt(?:[.-]|$)"),
    re.compile(r"^libcopt_cpp(?:[.-]|$)"),
    re.compile(r"^copt(?:[.-]|$)"),
    re.compile(r"^copt_cpp(?:[.-]|$)"),
)

COPT_SHIM_MEMBER_NAMES = frozenset(
    {
        "libsurge_copt_nlp.so",
        "libsurge_copt_nlp.dylib",
        "surge_copt_nlp.dll",
    }
)

ALLOWED_COPT_SHIM_PATTERNS = (
    re.compile(r"^libcopt(?:[.-]|$)"),
    re.compile(r"^libcopt_cpp(?:[.-]|$)"),
    re.compile(r"^copt(?:[.-]|$)"),
    re.compile(r"^copt_cpp(?:[.-]|$)"),
)


def read_needed(shared_object: pathlib.Path) -> list[str]:
    if shutil.which("readelf") is not None:
        return _read_needed_readelf(shared_object)
    if sys.platform == "darwin" and shutil.which("otool") is not None:
        return _read_needed_otool(shared_object)
    raise RuntimeError(
        "readelf (Linux) or otool (macOS) is required to inspect shared library linkage"
    )


def _read_needed_readelf(shared_object: pathlib.Path) -> list[str]:
    out = subprocess.check_output(["readelf", "-d", str(shared_object)], text=True)
    needed: list[str] = []
    for line in out.splitlines():
        if "Shared library:" not in line:
            continue
        start = line.index("[") + 1
        end = line.index("]", start)
        needed.append(line[start:end])
    return needed


def _read_needed_otool(shared_object: pathlib.Path) -> list[str]:
    out = subprocess.check_output(["otool", "-L", str(shared_object)], text=True)
    needed: list[str] = []
    for line in out.splitlines()[1:]:  # first line is the library itself
        line = line.strip()
        if not line:
            continue
        # otool format: "/path/to/lib.dylib (compatibility ...)"
        path = line.split(" (")[0].strip()
        needed.append(pathlib.Path(path).name)
    return needed


def allowed_patterns_for_member(member: str) -> tuple[re.Pattern[str], ...]:
    if pathlib.Path(member).name in COPT_SHIM_MEMBER_NAMES:
        return ALLOWED_COPT_SHIM_PATTERNS
    return ()


def forbidden(
    needed: list[str],
    allow_patterns: tuple[re.Pattern[str], ...] = (),
) -> list[str]:
    return [
        lib
        for lib in needed
        if any(pattern.match(lib) for pattern in FORBIDDEN_PATTERNS)
        and not any(pattern.match(lib) for pattern in allow_patterns)
    ]


def inspect_shared_object(
    path: pathlib.Path,
    allow_patterns: tuple[re.Pattern[str], ...] = (),
) -> int:
    needed = read_needed(path)
    bad = forbidden(needed, allow_patterns=allow_patterns)
    print(f"{path}:")
    for lib in needed:
        print(f"  NEEDED {lib}")
    if bad:
        print("  forbidden optional solver linkage detected:")
        for lib in bad:
            print(f"    {lib}")
        return 1
    return 0


def _is_native_member(member: str) -> bool:
    """Match native extension modules inside the surge package directory."""
    if not member.startswith("surge/"):
        return False
    return member.endswith(".so") or member.endswith(".dylib") or member.endswith(".pyd")


def inspect_wheel(path: pathlib.Path) -> int:
    rc = 0
    with tempfile.TemporaryDirectory() as td:
        root = pathlib.Path(td)
        with zipfile.ZipFile(path) as wheel:
            native_members = [m for m in wheel.namelist() if _is_native_member(m)]
            if not native_members:
                # Windows .pyd wheels cannot be inspected without dumpbin;
                # treat missing native members as a skip, not a failure.
                print(f"{path}: no native extension members found — skipping", file=sys.stderr)
                return 0
            for member in native_members:
                wheel.extract(member, root)
                extracted = root / member
                allow_patterns = allowed_patterns_for_member(member)
                # Skip inspection on platforms lacking the right tool
                # (e.g. .pyd on non-Windows, .dylib inspected on Linux).
                try:
                    rc |= inspect_shared_object(extracted, allow_patterns=allow_patterns)
                except RuntimeError as exc:
                    print(f"{path}: {exc} — skipping {member}", file=sys.stderr)
    return rc


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifacts", nargs="+", help="wheel or ELF shared object paths")
    args = parser.parse_args()

    rc = 0
    for artifact in args.artifacts:
        path = pathlib.Path(artifact)
        if not path.exists():
            print(f"{path}: does not exist", file=sys.stderr)
            rc = 1
            continue
        if path.suffix == ".whl":
            rc |= inspect_wheel(path)
        else:
            rc |= inspect_shared_object(path)
    return rc


if __name__ == "__main__":
    raise SystemExit(main())
