# c2-lakefile-lean-2: re-measure `lakefile.lean` support (C2), code unchanged

- **Date:** 2026-11-02
- **Author:** Claude (Opus 5.5), owner loop, machine `2e698309` (i9-14900K, 16 of 32 CPUs, 32 GB cap)
- **Milestone:** C2 (`lakefile.lean`)
- **Outcome:** failure (two more `failure` verdicts on fast-scenario noise; correct throughout)
- **Branch / PR:** code on `research/c2-lakefile-lean-2-wip` (draft PR https://github.com/adamtopaz/flume/pull/9, closed); this entry: `research/c2-lakefile-lean-2-log`
- **Result files:** research/results/20261103T045040Z-2e698309-72d1da8.json (5/20), research/results/20261103T061106Z-2e698309-4a9cbbf.json (5/40); same code as the two runs of `2026-11-02-c2-lakefile-lean.md`

## Plan

`2026-11-02-c2-lakefile-lean.md` implemented C2 (flume reads `lakefile.lean` through
`lake translate-config toml`, cached by content; constructs the translation would drop are
refused) and verified it against stock Lake, but both of its standard runs said `failure`,
each on one `noop` flag on a different corpus (and the corpus flagged in the second run had
"improved" 7.4% in the first). A direct measurement outside the harness showed no
difference (300 interleaved no-op builds: 15.09 ms vs 15.10 ms). The time budget ran out
before a third run.

This iteration re-measures that code unchanged, as `2026-11-02-p2-local-cache-2.md` did
for P2: branch from `research/c2-lakefile-lean-wip` with `main` merged in (`crates/` is
byte-identical to the wip branch's), verify (§5, plus `tests/check_lakefile_lean.py`), and
start the standard suite (`--repeat 5 --repeat-fast 20`, no `--targets`) at once, so that a
repeat would still fit. The new angle is the measurement only.

## What was done

- Branched from `research/c2-lakefile-lean-wip` and merged `main` (no conflicts); `crates/`
  identical to the wip branch's.
- Verified: `cargo build/test/clippy/fmt` clean (16 unit tests), `tests/check_headers.py`
  (2,546 files, 0 differences), `tests/check_cache.py`, `tests/check_zygote.py`, and
  `tests/check_lakefile_lean.py` (small-layered, custom package) ok; quick suite
  `identical artifacts to lake, all builds ok, break check ok`.
- Started the standard suite at 21:47 (load 18 at that moment from the quick suite just
  before), commit `72d1da8`.

## Measurements

### Run 1: standard suite, 5/20 repetitions

`research/results/20261103T045040Z-2e698309-72d1da8.json` (63 min):

```
speedup of candidate vs main: cold=1.001x, noop=0.941x, leaf=1.003x, newfile=0.993x, hub-private=0.993x, hub-interface=0.996x, swarm=1.004x, clone=0.998x, agents-4=0.978x, workflow:single=0.988x, workflow:many=0.988x, workflow:cold=1.001x, overall=0.993x
speedup of candidate vs lake: cold=1.117x, noop=4.048x, leaf=1.291x, newfile=1.804x, hub-private=1.433x, hub-interface=1.119x, swarm=1.221x, clone=1.762x, agents-4=1.469x, workflow:single=1.626x, workflow:many=1.609x, workflow:cold=1.117x, overall=1.430x
correctness of candidate: identical artifacts to lake, all builds ok, break check ok
verdict medium-deep/noop: regressed (paired speedup 0.884x over 20 runs, threshold 7%; medians main 0.04s, candidate 0.04s)
verdict medium-deep/clone: improved (paired speedup 1.141x over 20 runs, threshold 7%; medians main 0.18s, candidate 0.17s)
verdict medium-layered/swarm: improved (paired speedup 1.035x over 5 runs, threshold 3%; medians main 2.29s, candidate 2.28s)
verdict medium-wide/clone: regressed (paired speedup 0.880x over 20 runs, threshold 7%; medians main 0.18s, candidate 0.19s)
verdict: failure
```

(The other 27: `same`.) `noop` has now been flagged in all three runs of this code (layered
once, deep twice; deep "improved" 7.4% in the first), so I measured it again, this time the
way the harness does, each build in its own `systemd-run --user --scope -p MemoryMax=32G`,
150 interleaved pairs on a built medium-deep checkout, in the dev shell's environment:

```
main: median 57.4 ms, p10 41.6, p90 69.2
candidate: median 57.9 ms, p10 44.9, p90 71.9
paired median main/candidate: 0.994  fraction of pairs candidate slower: 0.53
```

Together with the earlier unscoped measurement (15.09 vs 15.10 ms), there is no cost. What
the runs show is the scope's own noise: ±25% per build (p10-p90 of 42-69 ms for the same
binary). A paired median of 20 such ratios has a spread of roughly ±7%, the size of the
threshold, so each sub-0.2 s comparison (four `noop`, three `clone`) flags by chance
something like one time in seven in each direction, and a run of a neutral change passes
only about a third of the time. That matches this machine's record: of the seven standard
runs of code changes so far, five said `failure`, each time on differences far larger than
anything the change costs there (P2's two runs, C2's three), and the two that passed (the
P2 re-measure and P1) drew no such flag.

### Run 2: the same with `--repeat-fast 40`

More samples are the only lever the rules give (fewer repetitions than the suite's make a
run non-standard; more do not), and they shrink that spread by about √2. Run 2 uses
`--repeat 5 --repeat-fast 40`.

`research/results/20261103T061106Z-2e698309-4a9cbbf.json` (100 min):

```
speedup of candidate vs main: cold=0.999x, noop=1.070x, leaf=0.994x, newfile=0.978x, hub-private=0.998x, hub-interface=0.996x, swarm=0.995x, clone=0.942x, agents-4=0.990x, workflow:single=1.005x, workflow:many=0.965x, workflow:cold=0.999x, overall=0.990x
speedup of candidate vs lake: cold=1.097x, noop=4.146x, leaf=1.417x, newfile=1.951x, hub-private=1.406x, hub-interface=1.105x, swarm=1.226x, clone=1.935x, agents-4=1.311x, workflow:single=1.672x, workflow:many=1.593x, workflow:cold=1.097x, overall=1.429x
correctness of candidate: identical artifacts to lake, all builds ok, break check ok
verdict medium-wide/clone: regressed (paired speedup 0.902x over 40 runs, threshold 7%; medians main 0.17s, candidate 0.20s)
verdict: failure
standard run: yes (suite standard)
```

(The other 30: `same`; no `noop` flag this time.) medium-wide/clone was also flagged in run
1 (0.880x), but the same code measured 1.060x there in the previous iteration's second run,
and the clone path of a `lakefile.toml` project differs from `main` by one failed `stat`.
The runs themselves show how noisy this scenario is: in this run Lake's 40 clone builds of
medium-wide are bimodal (0.24-0.29 s or 0.36-0.40 s), `main`'s spread from 0.13 to 0.22 s,
the candidate's from 0.13 to 0.23 s.

## Conclusion

- **C2's code is done and correct; its verdict is not.** Across four standard runs of
  identical code (two in the previous iteration, two in this one), artifacts
  were identical to Lake's every time, the held-out corpora passed, and
  `tests/check_lakefile_lean.py` shows Lean-configured corpora building exactly as stock
  Lake builds them. Every run said `failure`, each on a different combination of
  sub-0.2 s scenarios (layered/noop and layered/clone; deep/noop; deep/noop and wide/clone;
  wide/clone), while the same scenarios also "improved" by similar amounts in other runs.
  Measured directly, with and without the harness's systemd scope, the cost is zero.
- **The loop should stop here until the owner decides about the noise.** This is the second
  iteration in a row to end on the same failure, so by the loop's rules I do not start a
  third; and re-measuring again would test the dice, not the code. Options, all `bench:`
  changes for the owner to weigh (none is mine to make in a code PR, and each changes what
  `success` means for every later iteration):
  1. measure the tool's own wall time inside the scope (`scope_stats.sh` already wraps the
     command) instead of `systemd-run`'s, which removes the ±25% scope cost from every tool
     alike;
  2. for scenarios whose medians are under ~0.2 s, require a regression to exceed the noise
     the run itself measured (for example, the spread of `main`'s own runs) rather than a
     fixed 7%;
  3. or re-run only the flagged comparisons and require a flag to repeat.
- **Then:** re-measure `research/c2-lakefile-lean-2-wip` (or `research/c2-lakefile-lean-wip`;
  same code) with the new rule. Nothing in the code needs to change.
