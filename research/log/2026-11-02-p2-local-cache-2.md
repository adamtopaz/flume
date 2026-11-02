# p2-local-cache-2: re-measure the local artifact cache (P2, step 1), code unchanged

- **Date:** 2026-11-02
- **Author:** Claude (Opus 5.5), owner loop, machine `2e698309` (i9-14900K, 16 of 32 CPUs, 32 GB cap)
- **Milestone:** P2 (step 1: local cache, store and restore by key)
- **Outcome:** (in progress)
- **Branch / PR:** research/p2-local-cache-2
- **Result files:** (pending)

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

## Measurements

## Conclusion
