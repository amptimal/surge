# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import importlib.util
from pathlib import Path


SCRIPT_PATH = Path(__file__).resolve().parents[1] / "scripts" / "check_case_provenance.py"
SPEC = importlib.util.spec_from_file_location("check_case_provenance", SCRIPT_PATH)
assert SPEC is not None
assert SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def test_missing_provenance_reports_bundle(tmp_path):
    bundle_dir = tmp_path / "case9"
    bundle_dir.mkdir()
    bundle = bundle_dir / "case9.surge.json.zst"
    bundle.write_bytes(b"placeholder")

    missing = MODULE.missing_provenance(tmp_path)

    assert missing == [bundle]


def test_existing_provenance_passes(tmp_path):
    bundle_dir = tmp_path / "case9"
    bundle_dir.mkdir()
    (bundle_dir / "case9.surge.json.zst").write_bytes(b"placeholder")
    (bundle_dir / "PROVENANCE.md").write_text("# ok\n", encoding="utf-8")

    assert MODULE.missing_provenance(tmp_path) == []
