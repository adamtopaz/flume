#!/usr/bin/env python3
"""Check a pull request against the research-loop rules in instructions.md.

Usage (CI runs this; contributors run it before pushing):
  python3 ci/pr_check.py --base origin/main --title "milestone: <one line>"

"Code" is flume itself: crates/, worker/, Cargo.toml, Cargo.lock. Tests (tests/) may
change with code or on their own. The benchmark is bench/ (generator, harness, configs,
the Blake pin).

The PR title must start with one of these prefixes (case-insensitive):
  research:   a performance milestone: code + log entry + idea write-up + result(s);
              the newest result measured on this PR's code must say `success`
  milestone:  code that advances a roadmap milestone without claiming a speedup:
              log entry + result(s); the newest result on this PR's code must be
              correct and not `failure`
  log:        a failed or inconclusive iteration: log entry (+ results), no code
  bench:      the benchmark, CI, or test scripts; no code
  fix:        a code or infrastructure fix outside the research loop
  toolchain:  a deliberate change of the pinned Lean toolchain (lean-toolchain)
  ci:, docs:, chore:  infrastructure, documentation, housekeeping; no code

Universal rules: `lean-toolchain` is unchanged (except `toolchain:`); code and benchmark
never change in the same PR; committed results are standard runs measured on a clean
commit; existing results are never modified (except by a `bench:` rescore); the
generated leaderboard is never committed.

Exit status 1 with a list of problems, 0 when the PR passes.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

KINDS = ("research", "milestone", "log", "bench", "fix", "toolchain", "ci", "docs", "chore")
CODE_KINDS = ("research", "milestone", "fix", "toolchain")
INFRA_KINDS = ("bench", "ci", "fix", "chore")
CODE_PREFIXES = ("crates/", "worker/")
CODE_FILES = ("Cargo.toml", "Cargo.lock")
BENCH_PREFIXES = ("bench/configs/", "bench/gen/", "bench/harness/")
BENCH_FILES = ("bench/blake.json",)
INFRA_PREFIXES = (".github/", "ci/")
INFRA_FILES = ("flake.nix", "flake.lock")
RESULTS_DIR = "research/results/"
LOG_RE = r"research/log/\d{4}-\d{2}-\d{2}-.+\.md"
IDEA_RE = r"research/ideas/(?!README|TEMPLATE).+\.md"


def git(*args: str, check: bool = True) -> str:
    p = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True)
    if check and p.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {p.stderr.strip()}")
    return p.stdout


def show(ref: str, path: str) -> str | None:
    p = subprocess.run(["git", "show", f"{ref}:{path}"], cwd=ROOT, capture_output=True, text=True)
    return p.stdout if p.returncode == 0 else None


def changed_files(base: str) -> dict[str, str]:
    """{path: status letter} for files changed between the merge-base and HEAD (R = renamed-to)."""
    files: dict[str, str] = {}
    for line in git("diff", "--name-status", "--diff-filter=ACDMR", f"{base}...HEAD").splitlines():
        parts = line.split("\t")
        status = parts[0][0]
        if status == "R":
            files[parts[1]] = "D"
            files[parts[2]] = "R"
        else:
            files[parts[1]] = status
    return files


def is_code(p: str) -> bool:
    return p.startswith(CODE_PREFIXES) or p in CODE_FILES


def is_bench(p: str) -> bool:
    return p.startswith(BENCH_PREFIXES) or p in BENCH_FILES


def is_infra(p: str) -> bool:
    return p.startswith(INFRA_PREFIXES) or p in INFRA_FILES


def commit_exists(commit: str) -> bool:
    return subprocess.run(["git", "cat-file", "-e", f"{commit}^{{commit}}"], cwd=ROOT, capture_output=True).returncode == 0


def same_code_tree(commit: str) -> bool:
    p = subprocess.run(["git", "diff", "--quiet", commit, "HEAD", "--", *CODE_PREFIXES, *CODE_FILES],
                       cwd=ROOT, capture_output=True)
    return p.returncode == 0


def base_verdict(base: str, result: dict) -> str | None:
    """Recompute the result's verdict with the base branch's harness, if it has one."""
    run_src = show(base, "bench/harness/run.py")
    if run_src is None:
        return None
    with tempfile.TemporaryDirectory() as d:
        harness = Path(d) / "bench" / "harness"
        harness.mkdir(parents=True)
        (harness / "run.py").write_text(run_src, encoding="utf-8")
        edits = show(base, "bench/harness/edits.py")
        if edits is not None:
            (harness / "edits.py").write_text(edits, encoding="utf-8")
        spec = importlib.util.spec_from_file_location("base_run", harness / "run.py")
        mod = importlib.util.module_from_spec(spec)
        try:
            spec.loader.exec_module(mod)  # type: ignore[union-attr]
            return mod.verdict_for_result(result)["overall"]
        except Exception as e:  # noqa: BLE001
            return f"(could not recompute: {e})"


def result_correct(r: dict) -> bool:
    c = r.get("correctness", {}).get("candidate", {})
    return bool(c) and all(v.get("identical") and v.get("builds_ok") and v.get("break_ok") is not False
                           for v in c.values())


def check_result(path: str, base: str, problems: list[str], notes: list[str]) -> dict | None:
    text = show("HEAD", path)
    try:
        r = json.loads(text or "")
    except json.JSONDecodeError as e:
        problems.append(f"{path}: not readable JSON ({e})")
        return None
    if not r.get("standard"):
        problems.append(f"{path}: not a standard run ({', '.join(r.get('nonstandard_reasons', []))}); "
                        f"only standard runs may be committed")
    if r.get("git", {}).get("dirty"):
        problems.append(f"{path}: was measured on a dirty tree; commit before measuring")
    if r.get("verdict", {}).get("overall") is None:
        problems.append(f"{path}: has no verdict")
    h = r.get("harness", {})
    for key, rel in (("run_py_sha256", "bench/harness/run.py"), ("generate_py_sha256", "bench/gen/generate.py"),
                     ("edits_py_sha256", "bench/harness/edits.py")):
        base_src = show(base, rel)
        if base_src is not None and h.get(key) and h[key] != hashlib.sha256(base_src.encode("utf-8")).hexdigest():
            problems.append(f"{path}: measured with a {rel} that differs from {base}'s")
    for cfg_name, meta in r.get("corpora", {}).items():
        base_cfg = show(base, f"bench/configs/{cfg_name}.json")
        if base_cfg is not None and meta.get("config_sha256") and meta["config_sha256"] != hashlib.sha256(base_cfg.encode("utf-8")).hexdigest():
            problems.append(f"{path}: corpus config {cfg_name} differs from {base}'s")
    recomputed = base_verdict(base, r)
    if recomputed is not None and recomputed != r["verdict"]["overall"]:
        problems.append(f"{path}: stored verdict {r['verdict']['overall']!r} but {base}'s harness computes {recomputed!r}")
    correct = result_correct(r)
    notes.append(f"{path}: suite {r.get('suite', '?')}, verdict {r['verdict'].get('overall')}, candidate "
                 f"{'correct' if correct else 'INCORRECT'}, commit {r.get('git', {}).get('commit', '')[:7]}")
    r["_correct"] = correct
    return r


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base", default="origin/main", help="base ref the PR targets")
    ap.add_argument("--title", required=True, help="the PR title")
    args = ap.parse_args()

    if show(args.base, "ci/pr_check.py") is None:
        # The rules come from the base branch; a base without them (the founding PR) has
        # nothing to check against.
        print(f"{args.base} has no research-loop rules yet (ci/pr_check.py); nothing to check")
        return 0
    files = changed_files(args.base)
    m = re.match(r"\s*([A-Za-z]+):", args.title)
    kind = m.group(1).lower() if m else ""
    problems: list[str] = []
    notes: list[str] = []

    code = sorted(p for p in files if is_code(p))
    bench = sorted(p for p in files if is_bench(p))
    infra = sorted(p for p in files if is_infra(p))
    logs = sorted(p for p, s in files.items() if s in "AR" and re.fullmatch(LOG_RE, p))
    ideas = sorted(p for p, s in files.items() if s in "AR" and re.fullmatch(IDEA_RE, p))
    results_added = sorted(p for p, s in files.items() if s in "AR" and p.startswith(RESULTS_DIR) and p.endswith(".json"))
    results_touched = sorted(p for p, s in files.items() if s in "MD" and p.startswith(RESULTS_DIR) and p.endswith(".json"))

    if kind not in KINDS:
        problems.append(f"PR title must start with one of {', '.join(k + ':' for k in KINDS)} (got {args.title!r})")
    if "lean-toolchain" in files and kind != "toolchain":
        problems.append("lean-toolchain changed; use a `toolchain:` PR if this is deliberate")
    if code and kind not in CODE_KINDS:
        problems.append(f"`{kind or '(none)'}:` PR changes code ({', '.join(code[:5])}); only "
                        f"{', '.join(k + ':' for k in CODE_KINDS)} may")
    if code and bench:
        problems.append(f"code and benchmark changed in the same PR; split them (code: {', '.join(code[:3])}; "
                        f"bench: {', '.join(bench[:3])})")
    if bench and kind not in INFRA_KINDS:
        problems.append(f"`{kind or '(none)'}:` PR changes the benchmark ({', '.join(bench[:5])}); use `bench:`")
    if infra and kind not in INFRA_KINDS:
        problems.append(f"`{kind or '(none)'}:` PR changes CI or the dev shell ({', '.join(infra[:5])}); use `ci:` or `bench:`")
    if results_touched and kind != "bench":
        problems.append(f"existing results modified or deleted ({', '.join(results_touched[:5])}); results are immutable "
                        f"(a `bench:` PR may rescore them)")
    if f"{RESULTS_DIR}LEADERBOARD.md" in files:
        problems.append(f"{RESULTS_DIR}LEADERBOARD.md is generated after merge; do not commit it in a PR")

    results = {}
    for p in results_added:
        r = check_result(p, args.base, problems, notes)
        if r:
            results[p] = r

    if kind in ("research", "milestone"):
        if not code:
            problems.append(f"`{kind}:` PR changes no code; use `log:` for a result-only PR")
        if not logs:
            problems.append(f"`{kind}:` PR adds no research/log/YYYY-MM-DD-<slug>.md entry")
        if kind == "research" and not ideas:
            problems.append("`research:` PR adds no research/ideas/<slug>.md write-up")
        if not results_added:
            problems.append(f"`{kind}:` PR adds no research/results/*.json standard run")
        try:
            merge_base = git("merge-base", "HEAD", args.base).strip()
        except RuntimeError:
            merge_base = ""
        on_head = []
        for p, r in results.items():
            commit = r.get("git", {}).get("commit", "")
            if not commit_exists(commit):
                problems.append(f"{p}: measured commit {commit[:7]} is not in this repository; push it "
                                f"(merge origin/main into your branch rather than rebasing)")
            elif same_code_tree(commit):
                on_head.append((r["timestamp"], p, r))
            else:
                notes.append(f"{p}: measured on code that differs from this PR's; not decisive")
            main_commit = r.get("tools", {}).get("main", {}).get("commit", "")
            if main_commit and merge_base and main_commit != merge_base:
                notes.append(f"{p}: compared against main {main_commit[:7]}, but the PR's merge-base is now "
                             f"{merge_base[:7]}; re-measure if main changed the code")
        if results and not on_head:
            problems.append("no committed result was measured on this PR's code")
        if on_head:
            on_head.sort()
            _, newest_path, newest = on_head[-1]
            wanted = ("success",) if kind == "research" else ("success", "neutral")
            if newest["verdict"]["overall"] not in wanted:
                problems.append(f"the newest result on this PR's code ({newest_path}) has verdict "
                                f"{newest['verdict']['overall']!r}; a `{kind}:` PR needs {' or '.join(wanted)}")
            if not newest.get("_correct"):
                problems.append(f"the newest result on this PR's code ({newest_path}) is not correct")
            for _, p, r in on_head:
                if r["verdict"]["overall"] == "failure":
                    problems.append(f"{p}: a result on this PR's code has verdict `failure`")
    elif kind == "log" and not logs:
        problems.append("`log:` PR adds no research/log/YYYY-MM-DD-<slug>.md entry")

    print(f"PR kind: {kind or '(none)'}; {len(files)} changed files; code: {len(code)}; bench: {len(bench)}; "
          f"infra: {len(infra)}; log entries: {len(logs)}; ideas: {len(ideas)}; results: {len(results_added)}")
    for n in notes:
        print(f"  note: {n}")
    if problems:
        print("\nPROBLEMS:")
        for p in problems:
            print(f"  - {p}")
        return 1
    print("PR hygiene: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
