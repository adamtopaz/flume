#!/usr/bin/env python3
"""Run `<exe> build` in several checkouts at once and report each build.

The harness starts this script (through scope_stats.sh) inside one transient
systemd scope, so the memory cap and the CPU and memory accounting cover all
the concurrent builds together, as they would for agents sharing a machine.

Usage: multi_build.py OUT.json EXE DIR [DIR ...]
Writes {"makespan_s": ..., "builds": [{"dir", "wall_s", "exit_code", "log"}...]}
with each build's output in DIR/.bench-build.log.
"""

from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path


def main() -> int:
    out_path, exe, *dirs = sys.argv[1:]
    procs = []
    t0 = time.perf_counter()
    for d in dirs:
        log = open(Path(d) / ".bench-build.log", "w", encoding="utf-8")
        procs.append((d, subprocess.Popen([exe, "build"], cwd=d, stdout=log, stderr=subprocess.STDOUT), log,
                      time.perf_counter()))
    builds = []
    pending = list(procs)
    ends: dict[str, float] = {}
    while pending:
        still = []
        for item in pending:
            d, p, log, start = item
            if p.poll() is None:
                still.append(item)
            else:
                ends[d] = time.perf_counter() - start
                log.close()
        pending = still
        if pending:
            time.sleep(0.002)
    makespan = time.perf_counter() - t0
    worst = 0
    for d, p, _, _ in procs:
        builds.append({"dir": d, "wall_s": round(ends[d], 4), "exit_code": p.returncode})
        worst = worst or p.returncode
    Path(out_path).write_text(json.dumps({"makespan_s": round(makespan, 4), "builds": builds}), encoding="utf-8")
    return 1 if worst else 0


if __name__ == "__main__":
    sys.exit(main())
