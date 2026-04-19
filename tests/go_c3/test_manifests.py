# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

from pathlib import Path
import sys


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
if str(SCRIPTS_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_ROOT))

from benchmarks.go_c3.manifests import (
    load_dataset_manifest,
    load_native_case_manifest,
    load_suite_manifest,
)


def test_public_dataset_manifest_contains_event4_ladder():
    manifest = load_dataset_manifest()
    by_key = manifest.by_key()

    assert manifest.version == 1
    assert by_key["event4_73"].size_hint_buses == 73
    assert by_key["event4_617"].size_hint_buses == 617
    assert by_key["event4_23643"].size_hint_buses == 23643
    assert by_key["sandbox4_switching"].stage == "sandbox"


def test_suite_manifest_contains_expected_progression():
    manifest = load_suite_manifest()
    by_name = manifest.by_name()

    assert manifest.version == 1
    assert "smoke" in by_name
    assert "medium" in by_name
    assert by_name["smoke"].targets[0].dataset == "event4_73"
    assert by_name["smoke"].targets[0].scenario_ids == (303,)
    assert by_name["xlarge_manual"].targets[-1].dataset == "event4_23643"


def test_native_case_manifest_contains_core_triad_and_growth_plan():
    manifest = load_native_case_manifest()
    by_name = manifest.by_name()

    assert manifest.version == 1
    assert by_name["go_c3_event4_73_d1_303_sw0"].track_examples is True
    assert by_name["go_c3_event4_73_d2_911_sw0"].variants == ("full", "t00")
    assert by_name["go_c3_event4_73_d3_303_sw0"].tier == "core"
    assert by_name["go_c3_event4_617_d1_921_sw0"].tier == "medium"
    assert by_name["go_c3_event4_23643_d1_003_sw0"].tier == "xlarge_manual"
