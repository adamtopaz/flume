#!/usr/bin/env python3
"""Check flume's fork server for `lean` (the zygote) against `lean` itself.

Usage: python3 tests/check_zygote.py
Builds a small package twice, once with `FLUME_ZYGOTE=off` (every compile runs `lean`) and
once with the zygote, and checks that the zygote compiled modules (`-v` marks them), that
both builds wrote byte-identical artifacts, and that they reported the same exit status,
the same failed and blocked modules, and the same messages (a warning and an error). The
zygote is compiled with `cc` on first use, so a C compiler must be on PATH; build flume
first (`cargo build --release`). Exit status 1 on any failure.
"""
import hashlib
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FLUME = ROOT / "target" / "release" / "flume"

LAKEFILE = 'name = "pkg"\ndefaultTargets = ["Pkg"]\n\n[[lean_lib]]\nname = "Pkg"\nglobs = ["Pkg.+", "Legacy"]\n'
BASE = ("module\n\npublic section\n\n@[expose] def base (n : Nat) : Nat := n + 1\n\n"
        "theorem base_pos (n : Nat) : 0 < base n := by unfold base; omega\n\n"
        "theorem evens : (List.range 40).filter (fun k => k % 2 == 0) = "
        "[0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30, 32, 34, 36, 38] := by decide\n")
FILES = {
    "lakefile.toml": LAKEFILE,
    "Pkg/Base.lean": BASE,
    "Pkg/Ok.lean": "module\n\npublic import Pkg.Base\n\npublic section\n\ndef ok : Nat := base 41\n",
    "Pkg/Warn.lean": "module\n\npublic import Pkg.Base\n\npublic section\n\ntheorem warned : base 0 = 1 := by sorry\n",
    "Pkg/Err.lean": "module\n\npublic import Pkg.Base\n\npublic section\n\ndef bad : Nat := base \"x\"\n",
    "Pkg/Top.lean": "module\n\npublic import Pkg.Ok\npublic import Pkg.Warn\n\npublic section\n\ndef top : Nat := ok\n",
    "Pkg/Blocked.lean": "module\n\npublic import Pkg.Err\n\npublic section\n\ndef blocked : Nat := bad\n",
    "Legacy.lean": "import Pkg.Top\n\ndef legacy : Nat := top + 1\n",
}

failures: list[str] = []


def check(cond: bool, what: str, detail: str = "") -> None:
    print(("ok   " if cond else "FAIL ") + what + ("" if cond or not detail else "\n" + detail))
    if not cond:
        failures.append(what)


def checkout(base: Path, name: str) -> Path:
    d = base / name
    d.mkdir()
    for rel, text in FILES.items():
        (d / rel).parent.mkdir(parents=True, exist_ok=True)
        (d / rel).write_text(text, encoding="utf-8")
    shutil.copy(ROOT / "lean-toolchain", d / "lean-toolchain")
    return d


def build(d: Path, env: dict) -> tuple[int, str]:
    p = subprocess.run([str(FLUME), "-v", "build"], cwd=d, env=env, capture_output=True, text=True)
    return p.returncode, p.stdout + p.stderr


def normalized(out: str) -> list[str]:
    """The report without timings, counters, and the zygote marker, in a stable order."""
    lines = [re.sub(r" \(\d+ms\)( \[zygote\])?$", "", re.sub(r"\[\d+/\d+\] ", "", l))
             for l in out.splitlines()]
    return sorted(lines)


def artifacts(d: Path) -> dict[str, str]:
    build_dir = d / ".lake" / "build"
    return {str(p.relative_to(build_dir)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(build_dir.rglob("*")) if p.is_file() and not p.name.endswith(".setup.json")}


def main() -> int:
    if not FLUME.exists():
        print(f"build flume first: {FLUME} is missing")
        return 1
    if shutil.which("cc") is None:
        print("no C compiler (cc) on PATH: the zygote cannot be built here")
        return 1
    with tempfile.TemporaryDirectory(prefix="flume-zygote-check-") as tmp:
        base = Path(tmp)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("FLUME_", "LEAN_", "LAKE_"))}
        env["FLUME_CACHE_DIR"] = str(base / "cache")  # where the compiled zygote is kept
        off = dict(env, FLUME_ZYGOTE="off", FLUME_CACHE_DIR=str(base / "cache-off"))

        # the first build of a machine compiles the zygote in the background
        warm = checkout(base, "warm")
        (warm / "Pkg/Base.lean").write_text(BASE + "\ndef warm := 0\n", encoding="utf-8")
        build(warm, env)
        check(any((base / "cache").rglob("flume-zygote")), "the zygote was compiled into the cache")

        direct = checkout(base, "direct")
        rc_d, out_d = build(direct, off)
        check(rc_d == 1 and "[zygote]" not in out_d, "FLUME_ZYGOTE=off runs lean (and Err fails)", out_d)

        forked = checkout(base, "forked")
        rc_f, out_f = build(forked, env)
        n_forked = out_f.count("[zygote]")
        check(n_forked >= 3, f"the zygote compiled the modules after the first ({n_forked})", out_f)
        check(rc_f == rc_d, f"same exit status ({rc_f} vs {rc_d})")
        check(normalized(out_f) == normalized(out_d), "same report: failed, blocked, messages", f"{out_f}\n---\n{out_d}")
        check("declaration uses `sorry`" in out_f and "error: Pkg/Err.lean" in out_f,
              "the warning and the error are reported")
        a_d, a_f = artifacts(direct), artifacts(forked)
        check(len(a_d) == 4 * 7 + 3 and a_f == a_d, f"identical artifacts ({len(a_f)} files vs {len(a_d)})")

        # no process outlives the build
        left = subprocess.run(["pgrep", "-f", str(base / "cache")], capture_output=True, text=True).stdout.split()
        check(not left, f"no zygote left running ({left})")
    print("zygote check: " + ("ok" if not failures else f"{len(failures)} failure(s)"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
