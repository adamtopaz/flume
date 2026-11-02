# p2-local-cache-2: re-measure the local artifact cache (P2, step 1), code unchanged

- **Date:** 2026-11-02
- **Author:** Claude (Opus 5.5), owner loop, machine `2e698309` (i9-14900K, 16 of 32 CPUs, 32 GB cap)
- **Milestone:** P2 (step 1: local cache, store and restore by key)
- **Outcome:** success
- **Branch / PR:** research/p2-local-cache-2: https://github.com/adamtopaz/flume/pull/5
- **Result files:** research/results/20261102T225020Z-2e698309-15f5b59.json (this iteration); the code was measured before in research/results/20261102T200145Z-2e698309-976e41c.json and research/results/20261102T212100Z-2e698309-2ceaf64.json (both `failure`, see `2026-11-02-p2-local-cache.md`)

## Plan

`2026-11-02-p2-local-cache.md` built a content-addressed artifact cache shared by the
machine's checkouts and measured it twice with the standard suite. Both runs said
`failure`, each on a different regression that the other run did not repeat
(layered/noop, layered/swarm, wide/hub-interface; then legacy/leaf), while `clone` was
230-333x faster than `main`, artifacts were identical to Lake's everywhere, and the
held-out corpora passed. That entry's conclusion was to re-measure the code unchanged on
a quieter machine before concluding anything about its cost.

This iteration does exactly that, and nothing else:

- the branch starts from `research/p2-local-cache-wip` (so the commits the earlier results
  name stay reachable) with `main` merged in; the code (`crates/`) is byte-identical to the
  wip branch's;
- verification as in instructions.md §5 (build, tests, clippy, fmt, header oracle,
  `tests/check_cache.py`, quick suite), then the standard suite with
  `--targets clone,agents-4 --repeat 5 --repeat-fast 20` (the noise settings §6
  recommends, used from the start because the last two runs showed what 3/10 repetitions
  give on this machine), started while the machine is quiet (`uptime` load 2 at 15:26);
- if a scenario regresses again, compare it with the earlier runs: the same scenario
  regressing twice means a real cost to profile; a different one again means the
  measurement, which the log will say plainly.

The new angle is the measurement, not the code, and the earlier entry explains why that is
not a retry of a failed idea: no scenario lost twice.

## What was done

- Branched from `research/p2-local-cache-wip` and merged `main` (one add/add conflict in
  `2026-11-02-p2-local-cache.md`, resolved to `main`'s version); `crates/` is identical to
  the wip branch's, so this PR's code is what PR #3 measured.
- Verified: `cargo build/test/clippy/fmt` clean (14 unit tests), `tests/check_headers.py`
  (2,546 files, 0 differences), `tests/check_cache.py` ok, quick suite `identical
  artifacts to lake, all builds ok, break check ok` (clone 3.44x faster than Lake there).
- Ran the standard suite once with the noise settings, starting at 15:27 with the machine
  quieter than during the earlier runs (load 2 at the start; it rose to 15-17 at times, of
  which the run itself is up to 16); then the held-out quick suite on the merged code.
- Recorded the outcome: this entry, the idea write-up `research/ideas/p2-local-cache.md`,
  and P2's status in the roadmap (step 1 done, step 2 open).

## Measurements

Standard suite, `--targets clone,agents-4 --repeat 5 --repeat-fast 20`, CPUs 0-15, 32 GB,
80 minutes, commit `15f5b59`:

```
speedup of candidate vs main: cold=1.002x, noop=0.983x, leaf=0.990x, newfile=1.023x, hub-private=1.000x, hub-interface=0.995x, swarm=1.005x, clone=268.884x, agents-4=1.016x, workflow:single=0.999x, workflow:many=16.528x, workflow:cold=1.002x, overall=2.549x
speedup of candidate vs lake: cold=1.036x, noop=3.696x, leaf=1.392x, newfile=2.026x, hub-private=1.433x, hub-interface=1.042x, swarm=1.205x, clone=1.517x, agents-4=1.422x, workflow:single=1.630x, workflow:many=1.469x, workflow:cold=1.036x, overall=1.354x
correctness of candidate: identical artifacts to lake, all builds ok, break check ok
verdict medium-deep/clone: improved (paired speedup 232.731x over 5 runs, threshold 3%; medians main 44.61s, candidate 0.19s)
verdict medium-layered/clone: improved (paired speedup 267.767x over 5 runs, threshold 3%; medians main 48.33s, candidate 0.18s)
verdict medium-wide/clone: improved (paired speedup 321.337x over 5 runs, threshold 3%; medians main 45.92s, candidate 0.14s)
verdict: success [targets: clone, agents-4]
standard run: yes (suite standard)
```

Every other scenario on every corpus: `same` (paired speedups 0.952x to 1.058x, all within
their thresholds; the scenarios flagged in the earlier runs: layered/noop 1.031x,
layered/swarm 0.994x, wide/hub-interface 0.996x, legacy/leaf 0.982x). Medians, Lake / main /
candidate: `clone` 0.25 / 44.61 / 0.19 s (deep), 0.26 / 48.33 / 0.18 s (layered), 0.27 /
45.92 / 0.14 s (wide); `agents-4` 1.39 / 1.07 / 1.06 s, 0.64 / 0.40 / 0.39 s, 0.94 / 0.70 /
0.69 s.

Held-out quick suite (`.claude/heldout/`, three private corpora) on commit `f2e8a73`:
`correctness of candidate: identical artifacts to lake, all builds ok, break check ok`
(clone 2.181x faster than Lake there).

## Conclusion

- **P2 step 1 is done.** A fresh checkout of an already built project costs links, not
  compiles: `clone` went from 45-48 s to 0.14-0.19 s (233-321x faster than `main`, 1.5x
  faster than Lake's own artifact cache), the many-checkouts workflow from 0.09x to 1.47x
  of Lake, and the overall number from 0.53x to 1.35x of Lake. No other scenario moved.
- **The earlier `failure`s were the measurement.** Across three standard runs of the same
  code, four regressions were flagged once each and never again; this run, with 5/20
  repetitions on a quieter machine, flagged none. On this machine, use the noise settings
  from the start for any change expected to be neutral outside its targets, and look at
  `uptime`/`free` before a run.
- **`agents-4` did not move**, as expected: each agent's edit is unique, so its rebuilt
  modules are never in the cache. The cache pays off there only when agents' checkouts
  share work (several agents on one base revision building the same modules, or a new
  agent checkout), which `agents-16` in the milestone suite and P2 step 2 should look at.
- **Next for P2 (step 2):** a size bound with least-recently-used eviction (an `atime`-free
  record of use, e.g. touching the key entry on every hit), a setting for it, and the
  milestone suite for `agents-16`. The caveats in `research/ideas/p2-local-cache.md` (shared
  inodes make other checkouts re-hash restored outputs once; the cache is trusted like
  Lake's) are the places to look if a later run shows a cost.
