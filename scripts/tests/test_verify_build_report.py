"""Tests for verify-build-report.py.

Layer 1 — Unit (12 tests)
Layer 2 — Module end-to-end (2 tests)
Layer 3 — Integration with scenario-A divergence fixture (1 test)
Layer 4 — CLI usability (3 tests)
Layer 5 — Adversarial positive + negative controls (5 tests)

Total: 23 tests. Verifier block in plan-v3 locks this count.

Run: pytest scripts/tests/test_verify_build_report.py -v
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

SCRIPTS_DIR = Path(__file__).resolve().parent.parent
SCRIPT_PATH = SCRIPTS_DIR / "verify-build-report.py"
FIXTURES_DIR = Path(__file__).resolve().parent / "fixtures"

# Allow importing the verifier module for unit tests even though the filename
# has a hyphen (not a valid Python module identifier).
sys.path.insert(0, str(SCRIPTS_DIR))
import importlib.util

_spec = importlib.util.spec_from_file_location("verify_build_report", SCRIPT_PATH)
assert _spec is not None and _spec.loader is not None
vbr = importlib.util.module_from_spec(_spec)
# Register in sys.modules before exec so dataclass introspection can find it.
sys.modules["verify_build_report"] = vbr
_spec.loader.exec_module(vbr)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _write_plan(tmp_path: Path, body: str) -> Path:
    plan = tmp_path / "plan.md"
    plan.write_text(body, encoding="utf-8")
    return plan


def _write_spec(path: Path, test_count: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = ['import { test } from "vitest";', ""]
    for i in range(test_count):
        lines.append(f'test("case {i}", () => {{}});')
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def _run_cli(*args: str, cwd: Path | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPT_PATH), *args],
        capture_output=True,
        text=True,
        cwd=cwd,
    )


# ---------------------------------------------------------------------------
# Layer 1 — Unit tests (12)
# ---------------------------------------------------------------------------

def test_parse_verifier_block_valid(tmp_path: Path) -> None:
    plan = _write_plan(
        tmp_path,
        '# P\n\n## verifier: sub-scope X\n\n'
        '- unit_tests: 5 across 1 files matching tree/*.spec.ts\n'
        '- spec_files: ["a", "b"]\n'
        '- gate_scripts: []\n',
    )
    block = vbr.parse_plan(plan, "X")
    assert block.subscope == "X"
    assert len(block.counts) == 1
    assert block.counts[0].name == "unit_tests"
    assert block.counts[0].plan_count == 5
    assert block.counts[0].plan_files == 1
    assert block.counts[0].glob == "tree/*.spec.ts"
    assert block.lists["spec_files"].entries == ["a", "b"]
    assert block.lists["gate_scripts"].entries == []


def test_parse_verifier_block_missing(tmp_path: Path) -> None:
    plan = _write_plan(tmp_path, '# No verifier block here.\n')
    with pytest.raises(SystemExit) as exc:
        vbr.parse_plan(plan, "X")
    assert exc.value.code == 2


def test_parse_verifier_block_malformed_count_row(tmp_path: Path) -> None:
    plan = _write_plan(
        tmp_path,
        '## verifier: sub-scope X\n\n'
        '- unit_tests: many across 1 files matching tree/*.spec.ts\n',
    )
    with pytest.raises(SystemExit) as exc:
        vbr.parse_plan(plan, "X")
    assert exc.value.code == 2


def test_parse_verifier_block_malformed_spec_files_list(tmp_path: Path) -> None:
    plan = _write_plan(
        tmp_path,
        '## verifier: sub-scope X\n\n'
        '- spec_files: [not-a-valid-json]\n',
    )
    with pytest.raises(SystemExit) as exc:
        vbr.parse_plan(plan, "X")
    assert exc.value.code == 2


def test_glob_and_count_matches(tmp_path: Path) -> None:
    _write_spec(tmp_path / "tree" / "a.spec.ts", 3)
    _write_spec(tmp_path / "tree" / "b.spec.ts", 2)
    _write_spec(tmp_path / "tree" / "c.spec.ts", 4)
    claim = vbr.CountClaim(name="u", plan_count=9, plan_files=3, glob="tree/*.spec.ts")
    actual = vbr.enumerate_counts(claim, tmp_path, None)
    assert actual.actual_count == 9
    assert actual.actual_files == 3


def test_glob_case_sensitive(tmp_path: Path) -> None:
    # TEST( should NOT be counted when regex is case-sensitive.
    path = tmp_path / "tree" / "a.spec.ts"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text('TEST("upper", () => {});\ntest("lower", () => {});\n', encoding="utf-8")
    claim = vbr.CountClaim(name="u", plan_count=1, plan_files=1, glob="tree/*.spec.ts")
    actual = vbr.enumerate_counts(claim, tmp_path, None)
    assert actual.actual_count == 1


def test_glob_no_matches(tmp_path: Path) -> None:
    claim = vbr.CountClaim(name="u", plan_count=0, plan_files=0, glob="nonexistent/*.spec.ts")
    actual = vbr.enumerate_counts(claim, tmp_path, None)
    assert actual.actual_count == 0
    assert actual.actual_files == 0


def test_diff_matching_plan_and_tree_clean(tmp_path: Path) -> None:
    _write_spec(tmp_path / "a.spec.ts", 3)
    claim = vbr.CountClaim(name="u", plan_count=3, plan_files=1, glob="*.spec.ts")
    actual = vbr.enumerate_counts(claim, tmp_path, None)
    assert vbr.diff_counts(claim, actual) is None


def test_diff_higher_tree_count(tmp_path: Path) -> None:
    _write_spec(tmp_path / "a.spec.ts", 10)
    claim = vbr.CountClaim(name="u", plan_count=5, plan_files=1, glob="*.spec.ts")
    actual = vbr.enumerate_counts(claim, tmp_path, None)
    d = vbr.diff_counts(claim, actual)
    assert d is not None
    assert d.delta_count == 5


def test_diff_lower_tree_count(tmp_path: Path) -> None:
    _write_spec(tmp_path / "a.spec.ts", 2)
    claim = vbr.CountClaim(name="u", plan_count=5, plan_files=1, glob="*.spec.ts")
    actual = vbr.enumerate_counts(claim, tmp_path, None)
    d = vbr.diff_counts(claim, actual)
    assert d is not None
    assert d.delta_count == -3


def test_spec_files_missing_enumerated(tmp_path: Path) -> None:
    # Tree has only 1 of 3 claimed spec files.
    (tmp_path / "present.ts").write_text("", encoding="utf-8")
    claim = vbr.ListClaim(name="spec_files", entries=["present.ts", "absent1.ts", "absent2.ts"])
    actual = vbr.enumerate_list(claim, tmp_path, is_gate_list=False)
    assert "present.ts" in actual.present
    assert sorted(actual.missing) == ["absent1.ts", "absent2.ts"]


def test_gate_scripts_missing_enumerated(tmp_path: Path) -> None:
    (tmp_path / "gate1.sh").write_text("#!/bin/bash\n", encoding="utf-8")
    claim = vbr.ListClaim(name="gate_scripts", entries=["gate1.sh", "gate2.sh"])
    actual = vbr.enumerate_list(claim, tmp_path, is_gate_list=True)
    assert actual.present == ["gate1.sh"]
    assert actual.missing == ["gate2.sh"]


# ---------------------------------------------------------------------------
# Layer 2 — Module end-to-end (2)
# ---------------------------------------------------------------------------

def test_end_to_end_divergent(tmp_path: Path) -> None:
    _write_spec(tmp_path / "tree" / "a.spec.ts", 29)
    plan = _write_plan(
        tmp_path,
        '## verifier: sub-scope X\n\n'
        '- integration_tests: 50 across 8 files matching tree/*.spec.ts\n'
        '- spec_files: []\n'
        '- gate_scripts: []\n',
    )
    result = vbr.run(plan, "X", tmp_path, None)
    assert result == 1  # divergence


def test_end_to_end_clean(tmp_path: Path) -> None:
    _write_spec(tmp_path / "tree" / "a.spec.ts", 5)
    _write_spec(tmp_path / "tree" / "b.spec.ts", 5)
    plan = _write_plan(
        tmp_path,
        '## verifier: sub-scope X\n\n'
        '- integration_tests: 10 across 2 files matching tree/*.spec.ts\n'
        '- spec_files: []\n'
        '- gate_scripts: []\n',
    )
    result = vbr.run(plan, "X", tmp_path, None)
    assert result == 0  # clean


# ---------------------------------------------------------------------------
# Layer 3 — Integration with scenario-A divergence fixture (1)
# ---------------------------------------------------------------------------

def test_against_scenario_a_divergence_fixture() -> None:
    """Synthetic reproducer for a verifier mismatch: plan=50, tree=29."""
    fixture_dir = FIXTURES_DIR / "build_report_divergence_a"
    plan = fixture_dir / "plan-excerpt.md"
    tree_root = fixture_dir / "tree"
    assert plan.exists(), "missing scenario-A fixture plan"
    assert tree_root.exists(), "missing scenario-A fixture tree"

    result = vbr.run(plan, "A", tree_root, None)
    assert result == 1  # should detect divergence


# ---------------------------------------------------------------------------
# Layer 4 — CLI usability (3)
# ---------------------------------------------------------------------------

def test_cli_help_exits_zero() -> None:
    result = _run_cli("--help")
    assert result.returncode == 0
    assert "verify-build-report" in result.stdout.lower() or "usage" in result.stdout.lower()


def test_cli_missing_required_arg() -> None:
    result = _run_cli()
    assert result.returncode == 2
    assert "plan" in result.stderr.lower() or "required" in result.stderr.lower()


def test_cli_plan_path_not_found(tmp_path: Path) -> None:
    result = _run_cli("--plan", str(tmp_path / "nope.md"), "--subscope", "X")
    assert result.returncode == 2
    assert "not found" in result.stderr.lower()


# ---------------------------------------------------------------------------
# Layer 5 — Adversarial positive + negative controls (5)
# ---------------------------------------------------------------------------

def test_positive_control_missing_test_block(tmp_path: Path) -> None:
    """Planted violation: hand-remove a test block; verifier catches delta."""
    spec = tmp_path / "tree" / "a.spec.ts"
    _write_spec(spec, 3)
    # Remove one test line — now only 2 tests remain.
    content = spec.read_text(encoding="utf-8")
    content = content.replace('test("case 2", () => {});\n', "", 1)
    spec.write_text(content, encoding="utf-8")

    plan = _write_plan(
        tmp_path,
        '## verifier: sub-scope X\n\n'
        '- u: 3 across 1 files matching tree/*.spec.ts\n'
        '- spec_files: []\n'
        '- gate_scripts: []\n',
    )
    result = vbr.run(plan, "X", tmp_path, None)
    assert result == 1


def test_positive_control_missing_spec_file(tmp_path: Path) -> None:
    """Planted violation: claim two spec files, tree has one. Verifier catches."""
    (tmp_path / "alpha.ts").write_text("", encoding="utf-8")
    # No "beta.ts" written.
    plan = _write_plan(
        tmp_path,
        '## verifier: sub-scope X\n\n'
        '- spec_files: ["alpha.ts", "beta.ts"]\n'
        '- gate_scripts: []\n',
    )
    result = vbr.run(plan, "X", tmp_path, None)
    assert result == 1


def test_negative_control_matching_tree_is_clean(tmp_path: Path) -> None:
    """Non-empty populated lists + matching counts → exit 0, no false alarm."""
    _write_spec(tmp_path / "tree" / "a.spec.ts", 5)
    (tmp_path / "tree" / "a.spec.ts").exists()
    (tmp_path / "gate1.sh").write_text("#!/bin/bash\n", encoding="utf-8")
    plan = _write_plan(
        tmp_path,
        '## verifier: sub-scope X\n\n'
        '- u: 5 across 1 files matching tree/*.spec.ts\n'
        '- spec_files: ["a.spec.ts"]\n'
        '- gate_scripts: ["gate1.sh"]\n',
    )
    result = vbr.run(plan, "X", tmp_path, None)
    assert result == 0


def test_negative_control_both_lists_empty(tmp_path: Path) -> None:
    """Both lists empty, no count rows → valid no-op → exit 0."""
    plan = _write_plan(
        tmp_path,
        '## verifier: sub-scope X\n\n'
        '- spec_files: []\n'
        '- gate_scripts: []\n',
    )
    result = vbr.run(plan, "X", tmp_path, None)
    assert result == 0


def test_negative_control_spec_files_empty_gate_scripts_populated(tmp_path: Path) -> None:
    """Asymmetric: empty spec_files + gate_scripts present → exit 0 if gates exist."""
    (tmp_path / "only-gate.sh").write_text("#!/bin/bash\n", encoding="utf-8")
    plan = _write_plan(
        tmp_path,
        '## verifier: sub-scope X\n\n'
        '- spec_files: []\n'
        '- gate_scripts: ["only-gate.sh"]\n',
    )
    result = vbr.run(plan, "X", tmp_path, None)
    assert result == 0
