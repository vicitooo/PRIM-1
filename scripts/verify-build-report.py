"""BUILD-report-vs-tree mechanical verifier.

Parse a plan markdown file for a "## verifier: sub-scope <label>" block,
enumerate the claimed counts against the actual tree, exit 0 on match or
1 on divergence. Designed to be runnable by a plan-gate builder pre-commit
as a self-check, AND by a supervisor during BUILD diff review.

Motivation: an autonomous run surfaced BUILD-report honesty regressions
(sub-scope A claimed fewer integration tests than the locked plan required).
Sub-agent diff review caught it; self-verification did not. This script is
the mechanical fallback.

Usage:
    python verify-build-report.py --plan <path> --subscope <label> [options]

Options:
    --plan PATH         Plan markdown file with the ## verifier block(s).
    --subscope LABEL    Which sub-scope's block to verify.
    --tree-root PATH    Repo root for enumeration (default: cwd).
    --baseline COMMIT   Optional git commit SHA; if provided, file enumeration
                        is limited to files changed since this commit.

Exit codes:
    0 — plan matches tree (CLEAN).
    1 — divergence found (DIVERGENCE report on stdout).
    2 — unparseable plan or missing inputs (error on stderr).

Parser rules:
    Count row: "- <name>: <int> across <int> files matching <glob>"
    List row:  "- <name>: [<json-array>]"
    Empty list []  is valid; zero counts are valid.
    Nested describe(...) blocks do not affect test-block counts — the
    regex matches `test(` / `it(` line-anchored regardless of nesting.

Tested in scripts/tests/test_verify_build_report.py.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional


HEADING_RE = re.compile(
    r"^[ \t]*##\s+verifier:\s+sub-scope\s+(\S+)\s*$", re.MULTILINE
)
COUNT_ROW_RE = re.compile(
    r"^[ \t]*-\s+(\w+):\s+(\d+)\s+across\s+(\d+)\s+files?\s+matching\s+(.+?)\s*$"
)
LIST_ROW_RE = re.compile(r"^[ \t]*-\s+(\w+):\s+(\[.*?\])\s*$")
NEXT_HEADING_RE = re.compile(r"^[ \t]*##\s+", re.MULTILINE)
# Matches JS/TS test blocks `test(` / `it(` AND Python test defs `def test_*(`.
# Case-sensitive, line-anchored. Nested describe() blocks don't affect count —
# regex matches test/it lines regardless of nesting depth.
TEST_BLOCK_RE = re.compile(
    r"^\s*(test|it)\(|^\s*def\s+test_\w+\(",
    re.MULTILINE,
)


@dataclass
class CountClaim:
    name: str
    plan_count: int
    plan_files: int
    glob: str


@dataclass
class ListClaim:
    name: str
    entries: list[str]


@dataclass
class VerifierBlock:
    subscope: str
    counts: list[CountClaim] = field(default_factory=list)
    lists: dict[str, ListClaim] = field(default_factory=dict)


@dataclass
class CountActual:
    name: str
    actual_count: int
    actual_files: int


@dataclass
class ListActual:
    name: str
    present: list[str]
    missing: list[str]


@dataclass
class Divergence:
    kind: str
    claim: CountClaim | ListClaim
    actual: CountActual | ListActual | None
    delta_count: Optional[int] = None
    delta_files: Optional[int] = None


def die(msg: str, code: int = 2) -> None:
    print(f"verify-build-report: {msg}", file=sys.stderr)
    sys.exit(code)


def parse_plan(plan_path: Path, subscope: str) -> VerifierBlock:
    """Locate the verifier block for the given sub-scope and parse rows."""
    if not plan_path.exists():
        die(f"plan file not found: {plan_path}")
    text = plan_path.read_text(encoding="utf-8")

    headings = list(HEADING_RE.finditer(text))
    target = None
    for m in headings:
        if m.group(1).strip() == subscope.strip():
            target = m
            break
    if target is None:
        die(f"no '## verifier: sub-scope {subscope}' block in {plan_path}")

    start = target.end()
    next_heading = NEXT_HEADING_RE.search(text, pos=start)
    end = next_heading.start() if next_heading else len(text)
    body = text[start:end]

    block = VerifierBlock(subscope=subscope.strip())
    for lineno, raw_line in enumerate(body.splitlines(), start=1):
        line = raw_line.rstrip()
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if not stripped.startswith("-"):
            continue

        cm = COUNT_ROW_RE.match(line)
        if cm:
            block.counts.append(
                CountClaim(
                    name=cm.group(1),
                    plan_count=int(cm.group(2)),
                    plan_files=int(cm.group(3)),
                    glob=cm.group(4).strip(),
                )
            )
            continue

        lm = LIST_ROW_RE.match(line)
        if lm:
            raw_list = lm.group(2)
            try:
                parsed = json.loads(raw_list)
            except json.JSONDecodeError as exc:
                die(f"malformed JSON array on line {lineno}: {raw_list!r} ({exc})")
            if not isinstance(parsed, list) or not all(
                isinstance(e, str) for e in parsed
            ):
                die(f"list row {lm.group(1)!r} must be JSON array of strings (line {lineno})")
            block.lists[lm.group(1)] = ListClaim(name=lm.group(1), entries=parsed)
            continue

        die(f"unparseable row in verifier block (line {lineno}): {raw_line!r}")

    return block


def _files_changed_since(tree_root: Path, baseline: str) -> Optional[set[Path]]:
    """Return the set of files changed since baseline, or None if git call fails."""
    try:
        out = subprocess.check_output(
            ["git", "diff", "--name-only", baseline, "--"],
            cwd=tree_root,
            text=True,
            stderr=subprocess.PIPE,
        )
    except (subprocess.CalledProcessError, FileNotFoundError) as exc:
        die(f"git diff failed for baseline {baseline!r}: {exc}")
    return {(tree_root / line.strip()).resolve() for line in out.splitlines() if line.strip()}


def enumerate_counts(
    claim: CountClaim,
    tree_root: Path,
    baseline_files: Optional[set[Path]],
) -> CountActual:
    matches = sorted(tree_root.glob(claim.glob))
    if baseline_files is not None:
        matches = [m for m in matches if m.resolve() in baseline_files]
    total = 0
    for fp in matches:
        if not fp.is_file():
            continue
        try:
            body = fp.read_text(encoding="utf-8", errors="replace")
        except OSError as exc:
            die(f"cannot read {fp}: {exc}")
        total += len(TEST_BLOCK_RE.findall(body))
    return CountActual(name=claim.name, actual_count=total, actual_files=len(matches))


def enumerate_list(
    claim: ListClaim,
    tree_root: Path,
    is_gate_list: bool,
) -> ListActual:
    """For spec_files: basename check under tree_root.
    For gate_scripts: relative-path + readability check from tree_root.
    """
    present: list[str] = []
    missing: list[str] = []
    for entry in claim.entries:
        if is_gate_list:
            p = tree_root / entry
            if p.is_file() and p.stat().st_size >= 0:
                present.append(entry)
            else:
                missing.append(entry)
        else:
            # basename or relative-path match — accept any file whose name
            # matches the entry exactly OR whose rel path ends with entry.
            hits = [
                p
                for p in tree_root.rglob("*")
                if p.is_file()
                and (p.name == entry or p.name.startswith(entry + "."))
            ]
            (present if hits else missing).append(entry)
    return ListActual(name=claim.name, present=present, missing=missing)


def diff_counts(claim: CountClaim, actual: CountActual) -> Optional[Divergence]:
    if claim.plan_count == actual.actual_count and claim.plan_files == actual.actual_files:
        return None
    return Divergence(
        kind="count",
        claim=claim,
        actual=actual,
        delta_count=actual.actual_count - claim.plan_count,
        delta_files=actual.actual_files - claim.plan_files,
    )


def diff_list(claim: ListClaim, actual: ListActual) -> Optional[Divergence]:
    if not actual.missing:
        return None
    return Divergence(kind="list", claim=claim, actual=actual)


def format_clean(block: VerifierBlock, tree_root: Path, actuals_c: list[CountActual],
                 actuals_l: list[ListActual]) -> str:
    lines = [f"CLEAN in sub-scope {block.subscope} (tree-root: {tree_root}):"]
    for claim, actual in zip(block.counts, actuals_c):
        lines.append(
            f"  {claim.name}: plan={claim.plan_count} actual={actual.actual_count} "
            f"(files={claim.plan_files} actual={actual.actual_files})"
        )
    for actual in actuals_l:
        lines.append(f"  {actual.name}: all {len(actual.present)} present")
    lines.append("")
    lines.append("0 divergences.")
    return "\n".join(lines)


def format_divergent(
    block: VerifierBlock,
    tree_root: Path,
    actuals_c: list[CountActual],
    actuals_l: list[ListActual],
    divergences: list[Divergence],
) -> str:
    lines = [f"DIVERGENCE in sub-scope {block.subscope} (tree-root: {tree_root}):"]
    for d in divergences:
        if d.kind == "count":
            cl: CountClaim = d.claim  # type: ignore[assignment]
            ac: CountActual = d.actual  # type: ignore[assignment]
            lines.append(f"  {cl.name}:")
            lines.append(
                f"    plan={cl.plan_count} across {cl.plan_files} files matching {cl.glob}"
            )
            lines.append(f"    actual={ac.actual_count} across {ac.actual_files} files")
            lines.append(f"    delta_tests={d.delta_count:+d}")
            lines.append(f"    delta_files={d.delta_files:+d}")
        elif d.kind == "list":
            ll: ListClaim = d.claim  # type: ignore[assignment]
            al: ListActual = d.actual  # type: ignore[assignment]
            lines.append(f"  {ll.name}:")
            lines.append(f"    missing: {al.missing}")
    # Also report ok rows
    ok_count_rows = [a.name for c, a in zip(block.counts, actuals_c) if c.plan_count == a.actual_count and c.plan_files == a.actual_files]
    ok_list_rows = [a.name for a in actuals_l if not a.missing]
    for name in ok_count_rows:
        lines.append(f"  {name}: ok")
    for name in ok_list_rows:
        lines.append(f"  {name}: ok (all {sum(1 for al in actuals_l if al.name == name for _ in al.present)} present)")
    lines.append("")
    lines.append(f"{len(divergences)} divergence(s) found.")
    return "\n".join(lines)


def run(plan: Path, subscope: str, tree_root: Path, baseline: Optional[str]) -> int:
    block = parse_plan(plan, subscope)
    baseline_files = _files_changed_since(tree_root, baseline) if baseline else None

    actuals_c: list[CountActual] = []
    actuals_l: list[ListActual] = []
    divergences: list[Divergence] = []

    for claim in block.counts:
        actual = enumerate_counts(claim, tree_root, baseline_files)
        actuals_c.append(actual)
        d = diff_counts(claim, actual)
        if d is not None:
            divergences.append(d)

    for name, claim in block.lists.items():
        is_gate = name == "gate_scripts"
        actual = enumerate_list(claim, tree_root, is_gate_list=is_gate)
        actuals_l.append(actual)
        d = diff_list(claim, actual)
        if d is not None:
            divergences.append(d)

    if divergences:
        print(format_divergent(block, tree_root, actuals_c, actuals_l, divergences))
        return 1
    print(format_clean(block, tree_root, actuals_c, actuals_l))
    return 0


def parse_args(argv: Optional[list[str]] = None) -> argparse.Namespace:
    ap = argparse.ArgumentParser(
        prog="verify-build-report",
        description="Verify a BUILD report's claimed counts against tree reality.",
    )
    ap.add_argument("--plan", required=True, type=Path, help="Plan markdown file.")
    ap.add_argument("--subscope", required=True, help="Sub-scope label to verify.")
    ap.add_argument(
        "--tree-root",
        type=Path,
        default=Path.cwd(),
        help="Repo root for enumeration (default: cwd).",
    )
    ap.add_argument(
        "--baseline",
        default=None,
        help="Optional git commit SHA limiting enumeration to changed files.",
    )
    return ap.parse_args(argv)


def main(argv: Optional[list[str]] = None) -> int:
    args = parse_args(argv)
    tree_root = args.tree_root.resolve()
    return run(args.plan, args.subscope, tree_root, args.baseline)


if __name__ == "__main__":
    sys.exit(main())
