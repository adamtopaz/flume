# c2-lakefile-lean-2: re-measure `lakefile.lean` support (C2), code unchanged

- **Date:** 2026-11-02
- **Author:** Claude (Opus 5.5), owner loop, machine `2e698309` (i9-14900K, 16 of 32 CPUs, 32 GB cap)
- **Milestone:** C2 (`lakefile.lean`)
- **Outcome:** (in progress)
- **Branch / PR:** research/c2-lakefile-lean-2
- **Result files:** (pending)

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

## Measurements

## Conclusion
