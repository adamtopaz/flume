# p2-local-cache: a shared, content-addressed artifact cache on one machine (P2, step 1)

- **Date:** 2026-11-02
- **Author:** Claude (Opus 5.5), owner loop, machine `2e698309` (i9-14900K, 16 of 32 CPUs, 32 GB cap)
- **Milestone:** P2 (step 1: local cache, store and restore by key)
- **Outcome:** failure (harness verdict `failure` twice, on a different noise-level regression each time; `clone` 230-333x faster than `main`; correct throughout)
- **Branch / PR:** code on `research/p2-local-cache-wip` (draft PR https://github.com/adamtopaz/flume/pull/3, closed); this entry: `research/p2-local-cache-log`
- **Result files:** research/results/20261102T200145Z-2e698309-976e41c.json (3/10 repetitions), research/results/20261102T212100Z-2e698309-2ceaf64.json (5/20 repetitions); the two measured commits have the same code

## Plan

The baseline (`2026-11-02-baseline.md`) puts flume at 0.007x of Lake on `clone`: Lake
restores a fresh checkout from its artifact cache in ~0.3 s with 0 jobs, flume recompiles
all 1,010 modules in ~40 s. That one scenario drags the many-checkouts workflow to 0.09x
and the overall number to 0.54x. The baseline's `break` records show the same gap in a
smaller form: after a breaking edit is reverted, Lake restores the hub from its cache
(0.19 s, and 0 jobs in the repair step), flume recompiles it (0.7 s, 2 jobs).

Step 1 of P2 is a cache shared by every checkout on the machine, modeled on Lake's own
(`Cache.saveArtifact`, `restoreArtifact` in `Lake/Build/Common.lean`):

- **Where:** `$FLUME_CACHE_DIR`, else `$XDG_CACHE_HOME/flume`, else `~/.cache/flume`
  (the XDG convention every tool follows; `FLUME_CACHE_DIR=off` disables the cache).
- **What:** `art/<blake3 of content>`: each artifact once, by content; `key/<key>.json`:
  a module's outputs (facet, content hash, size) and its messages, by the module's key.
  The key already hashes everything Lake traces (toolchain, package, options, name,
  source, the imports' artifact traces), so equal keys mean Lake would treat the outputs
  as interchangeable; Lake's own cache uses the same input-hash-to-outputs mapping.
- **Store:** after a successful compile, each output is made read-only (as Lake does) and
  hard-linked into `art/` (copied through a temporary file and renamed if linking fails),
  then the key entry is written atomically.
- **Restore:** a module that is not up to date but whose key is in the cache gets its old
  outputs removed and every artifact hard-linked (or copied) into `.lake/build`, exactly
  where a compile would have put it, so the build directory keeps Lake's layout. It prints
  nothing (as Lake prints nothing for a replay), unless the stored messages contain
  warnings, which are replayed.

Targets: `clone` and `agents-4` (the standard suite cannot run `agents-16`; size bounds
and eviction are the next step of P2). I expect `clone` to drop from ~40 s to well under
a second and every other scenario to stay put: the store adds a link, a chmod, and one
small file per compiled module.

How I will know: the quick suite stays correct (identical artifacts after every build,
including the clone and the repair step), unit tests cover store/restore/fallbacks, and
the standard suite with `--targets clone,agents-4` says `success`.

## What was done

- `crates/flume/src/cache.rs` (new): the cache's location (`FLUME_CACHE_DIR`, else an
  absolute `XDG_CACHE_HOME` + `/flume`, else `~/.cache/flume`; then `v1/`;
  `FLUME_CACHE=off|0|false` disables it), `store` (read-only, hard link into
  `art/<h[..2]>/<h>` unless an intact copy of the same size is there, atomic entry under
  `key/<os>-<arch>/<k[..2]>/<k>.json`), `lookup`, `restore` (link or copy, size check).
  Unit tests: location rules, a round trip into a different directory, missing and
  wrong-size artifacts, unreadable entries.
- `crates/flume/src/build.rs`: `Shared::restore` runs after the up-to-date check and
  before compiling (lookup, `clear_outputs` as before a compile, link, stat);
  `Shared::compile` stores its outputs before taking their file status (chmod and link
  change the ctime); `Status::Restored`. A restored module prints nothing unless its
  stored messages hold a warning (`⚠ [i/n] Replayed M` with the messages), as Lake.
- `tests/check_cache.py` (new, needs Lean): a three-module package; a second checkout
  compiles nothing, replays the `sorry` warning, and gets identical artifacts; deleted
  artifacts are restored; an edit compiles one module; a cache with a missing and a
  grown artifact falls back to compiling exactly those modules and is repaired by it;
  `FLUME_CACHE=off` compiles everything.
- `research/DESIGN.md`: the cache in the architecture and a rule on why it is correct.

Surprises and decisions:
- Lake prints no `✔` line for a module restored from its cache, so `clone` and the
  repair step now show 0 jobs for flume too, matching Lake's counts in the harness (the
  baseline showed 1,010 and 2).
- A store first checks the cached file's *size*: a damaged cached file (wrong size) is
  replaced by the fresh compile's output, so the cache heals instead of failing the same
  restore forever. Content is not re-hashed on restore (Lake does not either); files are
  read-only, as Lake makes them.
- Store cost, measured with the same syscalls on this machine's XFS-on-dm-crypt disk:
  about 0.14 ms per compiled module, against compiles of 100+ ms.

Checks before measuring: `cargo build/test/clippy/fmt` clean, `tests/check_headers.py`
(2,546 files, 0 differences), `tests/check_cache.py` ok, quick suite and the held-out
quick suite both `identical artifacts to lake, all builds ok, break check ok` (clone
2.76x and 2.62x faster than Lake there).

## Measurements

### Run 1: standard suite, `--targets clone,agents-4`, CPUs 0-15, 32 GB, repetitions 3/10

`research/results/20261102T200145Z-2e698309-976e41c.json` (34 min):

```
speedup of candidate vs main: cold=0.994x, noop=0.913x, leaf=1.012x, newfile=0.983x, hub-private=1.036x, hub-interface=1.026x, swarm=0.995x, clone=267.511x, agents-4=0.992x, workflow:single=0.993x, workflow:many=16.294x, workflow:cold=0.994x, overall=2.524x
speedup of candidate vs lake: cold=1.062x, noop=3.806x, leaf=1.340x, newfile=1.932x, hub-private=1.496x, hub-interface=1.044x, swarm=1.186x, clone=1.565x, agents-4=1.349x, workflow:single=1.623x, workflow:many=1.453x, workflow:cold=1.062x, overall=1.358x
correctness of candidate: identical artifacts to lake, all builds ok, break check ok
verdict medium-deep/clone: improved (paired speedup 230.014x over 3 runs, threshold 3%; medians main 45.65s, candidate 0.20s)
verdict medium-layered/noop: regressed (paired speedup 0.877x over 10 runs, threshold 7%; medians main 0.04s, candidate 0.05s)
verdict medium-layered/swarm: regressed (paired speedup 0.967x over 3 runs, threshold 3%; medians main 1.97s, candidate 2.03s)
verdict medium-layered/clone: improved (paired speedup 281.791x over 3 runs, threshold 3%; medians main 43.69s, candidate 0.16s)
verdict medium-wide/hub-private: improved (paired speedup 1.087x over 10 runs, threshold 7%; medians main 0.45s, candidate 0.40s)
verdict medium-wide/hub-interface: regressed (paired speedup 0.969x over 3 runs, threshold 3%; medians main 44.52s, candidate 45.16s)
verdict medium-wide/clone: improved (paired speedup 304.721x over 3 runs, threshold 3%; medians main 41.90s, candidate 0.14s)
verdict: failure [targets: clone, agents-4]
```

(Every other scenario: `same`.) Why I read the three regressions as noise and reran:
- none of the three paths touches the cache except the store after each compile
  (~0.14 ms per module, i.e. a few ms for the 41 compiles of a swarm, against a 60 ms
  gap), and `noop` does not touch it at all;
- the same scenarios on the other corpora are `same`, on both sides of 1 (`noop`
  1.005x, 1.042x, 0.974x; `swarm` 1.009x, 1.009x; `hub-interface` 1.020x, 1.007x, 1.012x);
- `main`'s own runs of medium-wide/hub-interface spread over 8% (42.9 to 46.4 s), against
  a 3% threshold over 3 pairs; the noop medians (0.042 vs 0.049 s) are mostly the
  harness's systemd scope.

As instructions.md says for a result that looks like noise, I reran with
`--repeat 5 --repeat-fast 20`.

### Run 2: the same, repetitions 5/20

`research/results/20261102T212100Z-2e698309-2ceaf64.json` (78 min; commit `2ceaf64`,
whose code is `976e41c`'s):

```
speedup of candidate vs main: cold=1.005x, noop=1.021x, leaf=0.983x, newfile=0.990x, hub-private=1.010x, hub-interface=0.990x, swarm=1.005x, clone=307.768x, agents-4=1.009x, workflow:single=1.000x, workflow:many=17.620x, workflow:cold=1.005x, overall=2.606x
speedup of candidate vs lake: cold=1.036x, noop=4.017x, leaf=1.357x, newfile=1.957x, hub-private=1.416x, hub-interface=1.060x, swarm=1.234x, clone=1.913x, agents-4=1.499x, workflow:single=1.644x, workflow:many=1.694x, workflow:cold=1.036x, overall=1.423x
correctness of candidate: identical artifacts to lake, all builds ok, break check ok
verdict medium-deep/noop: improved (paired speedup 1.076x over 20 runs, threshold 7%; medians main 0.04s, candidate 0.04s)
verdict medium-deep/clone: improved (paired speedup 303.936x over 5 runs, threshold 3%; medians main 45.12s, candidate 0.15s)
verdict medium-layered/noop: same (paired speedup 0.976x over 20 runs, threshold 7%; medians main 0.04s, candidate 0.04s)
verdict medium-layered/swarm: same (paired speedup 0.992x over 5 runs, threshold 3%; medians main 2.28s, candidate 2.26s)
verdict medium-layered/clone: improved (paired speedup 304.059x over 5 runs, threshold 3%; medians main 46.86s, candidate 0.16s)
verdict medium-legacy/noop: improved (paired speedup 1.100x over 20 runs, threshold 7%; medians main 0.05s, candidate 0.04s)
verdict medium-legacy/leaf: regressed (paired speedup 0.898x over 20 runs, threshold 7%; medians main 0.45s, candidate 0.49s)
verdict medium-wide/hub-interface: same (paired speedup 1.015x over 5 runs, threshold 3%; medians main 42.48s, candidate 43.11s)
verdict medium-wide/clone: improved (paired speedup 333.138x over 5 runs, threshold 3%; medians main 44.24s, candidate 0.14s)
verdict: failure [targets: clone, agents-4]
```

(Every other scenario: `same`.) All three regressions of run 1 are gone, and `noop`
"improves" on two corpora with code that never touches the cache in a no-op build; a new
regression appeared in medium-legacy/leaf, which was 0.994x in run 1. Its runs:

```
main       0.450 0.428 0.427 0.424 0.425 0.431 0.456 0.487 0.457 0.425 0.459 0.457 0.445 0.443 0.447 0.438 0.455 0.448 0.447 0.463
candidate  1.029 0.528 0.502 0.484 0.496 0.511 0.470 0.542 0.509 0.541 0.512 0.513 0.464 0.475 0.458 0.467 0.459 0.480 0.471 0.473
lake       1.272 0.652 0.652 0.652 0.651 0.577 0.771 0.672 0.793 0.652 0.767 0.671 0.784 0.676 0.681 0.672 0.674 0.783 0.788 0.780
```

The candidate starts slow and converges on `main` (paired ratios 0.81 at the start,
0.93-0.99 at the end); the work per build is identical (one compile, one store of three
files, ~0.1 ms). On this corpus the tools' cold builds ran in the order candidate, lake,
main, so the candidate's 1,000 `.olean` files (which the leaf, an ordinary file, imports
transitively) were the oldest in the page cache, and Lake's first leaf build was slow too
(1.27 s, then 0.65 s). The machine was not quiet: it is a shared workstation (k3s, browsers, other coding
agents on the CPUs outside the cap) with 62 GB of RAM, 21 GB of swap in use and 15 GB
free when the run ended, and a 15-minute load average of 25 at its end (the run itself
accounts for up to 16). Evicted artifacts of the tool that built first are the most
likely explanation; I found no mechanism by which the cache could slow a leaf build.

I did not run a third time: it would take another 80 minutes past this iteration's time
budget, with each run so far producing a false regression somewhere among its 31
comparisons. Following the rules, the harness's `failure` stands and the code goes to a
`-wip` branch.

## Conclusion

- **The cache does what P2 asks.** `clone` dropped from 42-47 s to 0.14-0.20 s on every
  corpus (230-333x faster than `main`, 1.6-1.9x faster than Lake's own cache), the many
  checkouts workflow went from 0.09x to 1.45-1.69x of Lake and the overall number from
  0.54x to 1.36-1.42x, and the repair step after deleted artifacts now restores from the
  cache with 0 jobs, as Lake does. Artifacts were identical to Lake's after every build
  of both standard runs, the quick suite, and the held-out corpora.
- **It failed on measurement noise, not on cost.** Two standard runs flagged four
  different regressions (layered/noop, layered/swarm, wide/hub-interface, then
  legacy/leaf), none of which repeated, on paths the cache touches with ~0.1 ms of work or
  not at all.
- **Next iteration: re-measure `research/p2-local-cache-wip` as it is** (merge `main` in
  first if it moved, commit, then `--targets clone,agents-4`). That is not a retry of a
  failed idea without a new angle: the idea never lost a comparison twice; only the
  verdict did, once per run, on a different scenario. Prefer a quiet machine (check
  `uptime` and `free` first). If one scenario regresses in two runs, profile it before
  concluding anything.
- **For the harness owner:** with 31 comparisons per standard run, 3-repetition 3%
  thresholds on 40 s scenarios whose own runs spread 8%, and a shared machine, a false
  `regressed` per run is likely. Interleaving the cold builds' order per corpus does not
  help scenarios that read files written long before; a longer `--repeat` only for the
  scenarios a run flagged, or a pause/drop-caches step before each scenario, would be a
  `bench:` change worth considering.
- Step 2 of P2 (a size bound with eviction, and the milestone suite for agents-16) waits
  for step 1. The cache's caveats (no size bound; hard links share inodes, so restoring
  into one checkout makes others re-hash those outputs once) are in the wip branch's
  `research/ideas/p2-local-cache.md` draft.
