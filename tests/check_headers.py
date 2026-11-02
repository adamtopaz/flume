#!/usr/bin/env python3
"""Check flume's Rust header parser against Lean's own (via flume-worker).

Usage: python3 tests/check_headers.py [PATH...]
Every .lean file under each PATH (default: the stock toolchain's Lean sources, about
5,000 real headers) is parsed by both; any difference in module flag, imports, or
whether parsing failed is reported. Exit status 1 on any difference.
Build both first: `cargo build --release` and `(cd worker && lake build)`.
"""
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FLUME = ROOT / "target" / "release" / "flume"
WORKER = ROOT / "worker" / ".lake" / "build" / "bin" / "flume-worker"


def parse_all(cmd: list[str], files: list[str]) -> dict[str, dict]:
    out: dict[str, dict] = {}
    for i in range(0, len(files), 500):
        p = subprocess.run(cmd + files[i:i + 500], capture_output=True, text=True, check=True, cwd=ROOT)
        for line in p.stdout.splitlines():
            r = json.loads(line)
            out[r.pop("file")] = r
    return out


def main() -> int:
    paths = [Path(a) for a in sys.argv[1:]]
    if not paths:
        prefix = subprocess.run(["lean", "--print-prefix"], capture_output=True, text=True, check=True, cwd=ROOT).stdout.strip()
        paths = [Path(prefix) / "src" / "lean"]
    files = sorted(str(f) for p in paths for f in (p.rglob("*.lean") if p.is_dir() else [p]))
    lean = parse_all([str(WORKER), "headers"], files)
    mine = parse_all([str(FLUME), "debug-headers"], files)
    bad = 0
    for f in files:
        a, b = lean.get(f), mine.get(f)
        same = (("error" in a) == ("error" in b)) and ("error" in a or a == b)
        if not same:
            bad += 1
            if bad <= 10:
                print(f"DIFFERENT {f}\n  lean:  {json.dumps(a)}\n  flume: {json.dumps(b)}")
    print(f"{len(files)} files, {bad} differences")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
