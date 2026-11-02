#!/usr/bin/env python3
"""Check flume's shared artifact cache end to end on a tiny package.

Usage: python3 tests/check_cache.py
Builds a three-module package in one checkout, then checks that other checkouts of the
same sources sharing the cache (`FLUME_CACHE_DIR`) get byte-identical artifacts without
compiling, that warnings are replayed, that deleted artifacts are restored, that an
edited module is compiled, that a damaged cache falls back to compiling, and that
`FLUME_CACHE=off` compiles everything. Build flume first (`cargo build --release`);
`lean` must be on PATH or found through elan. Exit status 1 on any failure.
"""
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FLUME = ROOT / "target" / "release" / "flume"

FILES = {
    "lakefile.toml": 'name = "pkg"\ndefaultTargets = ["Pkg"]\n\n[[lean_lib]]\nname = "Pkg"\nglobs = ["Pkg.+"]\n',
    "Pkg/Base.lean": "module\n\npublic section\n\n@[expose] def base (n : Nat) : Nat := n + 1\n\n"
                     "theorem base_pos (n : Nat) : 0 < base n := by unfold base; omega\n",
    "Pkg/Warn.lean": "module\n\npublic import Pkg.Base\n\npublic section\n\n"
                     "theorem warned : base 0 = 1 := by sorry\n",
    "Pkg/Top.lean": "module\n\npublic import Pkg.Base\npublic import Pkg.Warn\n\npublic section\n\n"
                    "def top : Nat := base 41\n",
}

failures: list[str] = []


def check(cond: bool, what: str) -> None:
    print(("ok   " if cond else "FAIL ") + what)
    if not cond:
        failures.append(what)


def build(d: Path, env: dict) -> tuple[int, str]:
    p = subprocess.run([str(FLUME), "build"], cwd=d, env=env, capture_output=True, text=True)
    return p.returncode, p.stdout + p.stderr


def compiled(out: str) -> int:
    return sum(1 for line in out.splitlines() if line.startswith(("✔", "⚠")) and "] Built " in line)


def artifacts(d: Path) -> dict[str, str]:
    build_dir = d / ".lake" / "build"
    return {str(p.relative_to(build_dir)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(build_dir.rglob("*"))
            if p.is_file() and not p.name.endswith((".setup.json", ".hash", ".trace"))}


def checkout(base: Path, name: str) -> Path:
    d = base / name
    d.mkdir()
    for rel, text in FILES.items():
        (d / rel).parent.mkdir(parents=True, exist_ok=True)
        (d / rel).write_text(text, encoding="utf-8")
    shutil.copy(ROOT / "lean-toolchain", d / "lean-toolchain")
    return d


def main() -> int:
    if not FLUME.exists():
        print(f"build flume first: {FLUME} is missing")
        return 1
    with tempfile.TemporaryDirectory(prefix="flume-cache-check-") as tmp:
        base = Path(tmp)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("FLUME_", "LEAN_", "LAKE_"))}
        env["FLUME_CACHE_DIR"] = str(base / "cache")

        a = checkout(base, "a")
        rc, out = build(a, env)
        check(rc == 0 and compiled(out) == 3 and "Built Pkg.Warn" in out, f"cold build compiles everything\n{out}")
        ref = artifacts(a)
        check(len(ref) == 3 * 7, f"cold build writes 21 artifacts (found {len(ref)})")

        b = checkout(base, "b")
        rc, out = build(b, env)
        check(rc == 0 and compiled(out) == 0, f"a second checkout compiles nothing\n{out}")
        check("⚠" in out and "Replayed Pkg.Warn" in out and "sorry" in out,
              f"the cached module's warning is replayed\n{out}")
        check(artifacts(b) == ref, "its artifacts are identical to the first checkout's")
        rc, out = build(b, env)
        check(rc == 0 and "Replayed" not in out and compiled(out) == 0, f"the next build is a no-op\n{out}")

        (b / ".lake/build/ir/Pkg/Base.c").unlink()
        (b / ".lake/build/lib/lean/Pkg/Top.olean.server").unlink()
        rc, out = build(b, env)
        check(rc == 0 and compiled(out) == 0 and artifacts(b) == ref, f"deleted artifacts are restored\n{out}")

        top = b / "Pkg/Top.lean"
        top.write_text(top.read_text(encoding="utf-8") + "\ndef top2 : Nat := top + 1\n", encoding="utf-8")
        rc, out = build(b, env)
        check(rc == 0 and compiled(out) == 1 and "Built Pkg.Top" in out, f"an edited module is compiled\n{out}")

        # damage the cache: Base's .olean goes missing, Top's .c changes size (cache files
        # are found by inode: the first checkout's artifacts are hard links to them)
        cached = {p.stat().st_ino: p for p in (base / "cache" / "v1" / "art").rglob("*") if p.is_file()}
        missing = cached[(a / ".lake/build/lib/lean/Pkg/Base.olean").stat().st_ino]
        grown = cached[(a / ".lake/build/ir/Pkg/Top.c").stat().st_ino]
        missing.unlink()
        tmp = grown.with_name("grown")
        tmp.write_bytes(grown.read_bytes() + b"!")
        tmp.replace(grown)  # not in place: the checkouts' hard links keep the original
        c = checkout(base, "c")
        rc, out = build(c, env)
        check(rc == 0 and compiled(out) == 2 and "Built Pkg.Base" in out and "Built Pkg.Top" in out,
              f"a damaged cache falls back to compiling\n{out}")
        check(artifacts(c) == ref, "with identical artifacts")
        e = checkout(base, "e")
        rc, out = build(e, env)
        check(rc == 0 and compiled(out) == 0 and artifacts(e) == ref, f"and the compiles repaired the cache\n{out}")

        off = dict(env, FLUME_CACHE="off")
        d = checkout(base, "d")
        rc, out = build(d, off)
        check(rc == 0 and compiled(out) == 3 and artifacts(d) == ref, f"FLUME_CACHE=off compiles everything\n{out}")
    print("cache check: " + ("ok" if not failures else f"{len(failures)} failure(s)"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
