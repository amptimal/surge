# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import importlib.util
from pathlib import Path
import zipfile


SCRIPT_PATH = Path(__file__).resolve().parents[1] / "scripts" / "check_wheel_legal_files.py"
SPEC = importlib.util.spec_from_file_location("check_wheel_legal_files", SCRIPT_PATH)
assert SPEC is not None
assert SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def write_wheel(path: Path, members: dict[str, bytes]) -> None:
    with zipfile.ZipFile(path, "w") as wheel:
        for member, payload in members.items():
            wheel.writestr(member, payload)


def test_missing_required_patterns_are_reported():
    members = ["surge_py-0.1.0.dist-info/licenses/LICENSE"]

    assert MODULE.missing_required_patterns(members) == ["NOTICE", "SBOM"]


def test_vendor_copt_runtime_members_are_rejected():
    members = [
        "surge/libsurge_copt_nlp.dylib",
        "surge/libcopt_cpp.dylib",
        "surge/libcopt.dylib",
    ]

    assert MODULE.forbidden_distributed_members(members) == [
        "surge/libcopt_cpp.dylib",
        "surge/libcopt.dylib",
    ]


def test_inspect_wheel_accepts_required_legal_members(tmp_path):
    wheel = tmp_path / "surge_py-0.1.0-py3-none-any.whl"
    write_wheel(
        wheel,
        {
            "surge/_surge.abi3.so": b"",
            "surge_py-0.1.0.dist-info/licenses/LICENSE": b"license",
            "surge_py-0.1.0.dist-info/licenses/NOTICE": b"notice",
            "surge_py-0.1.0.dist-info/sboms/surge-py.cyclonedx.json": b"{}",
        },
    )

    assert MODULE.inspect_wheel(wheel, ()) == 0
