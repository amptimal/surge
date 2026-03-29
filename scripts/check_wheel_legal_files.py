#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Fail if built wheels are missing required legal metadata files."""

from __future__ import annotations

import argparse
import pathlib
import re
import sys
import zipfile


REQUIRED_PATTERNS = {
    "LICENSE": re.compile(r"^[^/]+\.dist-info/licenses/LICENSE$"),
    "NOTICE": re.compile(r"^[^/]+\.dist-info/licenses/NOTICE$"),
    "SBOM": re.compile(r"^[^/]+\.dist-info/sboms/.+\.json$"),
}

FORBIDDEN_DISTRIBUTED_MEMBER_PATTERNS = (
    re.compile(r"(^|/)libcopt(?:[.-]|$)"),
    re.compile(r"(^|/)libcopt_cpp(?:[.-]|$)"),
    re.compile(r"(^|/)copt(?:[.-]|$)"),
    re.compile(r"(^|/)copt_cpp(?:[.-]|$)"),
)

ALLOWED_DISTRIBUTED_MEMBER_NAMES = frozenset(
    {
        "libsurge_copt_nlp.so",
        "libsurge_copt_nlp.dylib",
        "surge_copt_nlp.dll",
    }
)


def native_members(members: list[str]) -> list[str]:
    return [
        member
        for member in members
        if member.startswith("surge/")
        and member.endswith((".so", ".dylib", ".pyd", ".dll"))
    ]


def missing_required_patterns(members: list[str]) -> list[str]:
    missing: list[str] = []
    for label, pattern in REQUIRED_PATTERNS.items():
        if not any(pattern.match(member) for member in members):
            missing.append(label)
    return missing


def missing_explicit_members(
    members: list[str], required_members: tuple[str, ...]
) -> list[str]:
    member_set = set(members)
    return [member for member in required_members if member not in member_set]


def forbidden_distributed_members(members: list[str]) -> list[str]:
    bad: list[str] = []
    for member in members:
        name = pathlib.Path(member).name
        if name in ALLOWED_DISTRIBUTED_MEMBER_NAMES:
            continue
        if any(pattern.search(member) for pattern in FORBIDDEN_DISTRIBUTED_MEMBER_PATTERNS):
            bad.append(member)
    return bad


def inspect_wheel(path: pathlib.Path, required_members: tuple[str, ...]) -> int:
    with zipfile.ZipFile(path) as wheel:
        members = wheel.namelist()

    rc = 0
    missing = missing_required_patterns(members)
    if missing:
        rc = 1
        print(f"{path}: missing required legal payloads: {', '.join(missing)}")

    missing_members = missing_explicit_members(members, required_members)
    if missing_members:
        rc = 1
        print(f"{path}: missing required wheel members:")
        for member in missing_members:
            print(f"  {member}")

    forbidden_members = forbidden_distributed_members(members)
    if forbidden_members:
        rc = 1
        print(f"{path}: wheel must not redistribute vendor COPT runtime members:")
        for member in forbidden_members:
            print(f"  {member}")

    bundled_native = native_members(members)
    if bundled_native:
        print(f"{path}: bundled native members:")
        for member in bundled_native:
            print(f"  {member}")
    else:
        print(f"{path}: no bundled native members found")

    if rc == 0:
        print(f"{path}: legal metadata ok")
    return rc


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifacts", nargs="+", help="wheel paths to inspect")
    parser.add_argument(
        "--require-member",
        action="append",
        default=[],
        help="exact wheel member path that must exist",
    )
    args = parser.parse_args()

    rc = 0
    required_members = tuple(args.require_member)
    for artifact in args.artifacts:
        path = pathlib.Path(artifact)
        if not path.exists():
            print(f"{path}: does not exist", file=sys.stderr)
            rc = 1
            continue
        rc |= inspect_wheel(path, required_members)
    return rc


if __name__ == "__main__":
    raise SystemExit(main())
