# Blake benchmarks

Everything here exists to answer one question reproducibly: *is this version of
Blake faster than the last one, and than stock Lake, on builds that look like a
gigantic Lean project, without doing anything Lake would not do?* See
`../instructions.md` for how the research loop uses it.

## Pieces

- `gen/generate.py`: generates a self-contained Lean package (imports only Lean
  core) with a controlled import DAG and controlled per-module elaboration cost.
  Deterministic: the same config always yields byte-identical files.
- `configs/*.json`: the corpora. Three DAG shapes at three sizes:
  - `layered`: random layered DAG, bushy, depth about the square root of the module count.
  - `deep`: a handful of long chains with occasional cross links; long critical path.
  - `wide`: a small core of hub modules imported by nearly everything, then a wide
    shallow fan; editing a hub rebuilds almost the whole corpus.
  - sizes: `small` (200 modules, smoke tests), `medium` (1000 modules, the
    standard sweep), `large` (8000 modules, for scaling questions; slow).
- `harness/run.py`: the measurement. Resolves stock Lake from the pinned toolchain
  (`$(lean --print-prefix)/bin/lake`), builds Blake `main` (a git worktree of the
  merge-base of `HEAD` and `origin/main`) and the candidate (this checkout), copies
  the corpus once per tool, runs the scenarios below, checks correctness, decides
  the verdict, and writes one JSON result.
- `harness/scope_stats.sh`: runs each build inside its systemd scope and records
  the scope's peak memory and CPU time, so helper processes are accounted for.
- `harness/leaderboard.py`: renders `../research/results/LEADERBOARD.md` (the
  Leaderboard workflow runs it after every merge that adds results).
- `local.json` (gitignored): your machine's caps, e.g. `{"cpus": "0-15", "mem_gb": 32}`.
- `work/` (gitignored): generated corpora, per-tool copies, per-tool caches, the
  `main` worktree, and results of non-standard runs.

## Scenarios

| scenario | what happens | what it measures |
|---|---|---|
| `cold` | build from nothing | total throughput; the scheduler's ability to keep the CPUs busy |
| `noop` | build again with nothing changed | pure build-tool overhead: loading the workspace, trace and hash checks |
| `leaf` | append a theorem to a module nobody imports, build | minimal end-to-end latency of one edit |
| `swarm` | append a theorem to up to 24 little-imported modules spread over the DAG, build | what a swarm of agents produces between builds |
| `hub` | append a theorem to the most widely imported module, build | a large downstream rebuild: scheduling and parallelism |
| `break` | rename the hub's `val_ge`, build (must fail), restore, build | untimed; every dependent must still be rebuilt when it must |

Edits accumulate (each one is a new theorem with a fresh name), so the tools see the
same sequence of edits and no restore step is needed. A scenario whose first run
takes under 2 s is run `--repeat-fast` times (default 10), a longer one `--repeat`
times (default 3); tables report medians. The tools are **interleaved**: every
repetition applies the same edit to every tool's copy and builds them one after
another in a rotating order, so drift of the machine over time (thermal state,
caches, background load) affects all tools alike instead of whichever ran last.

## Caps and comparability

The harness pins itself and all children to `--cpus` (CPU affinity; Linux only)
and sets `LEAN_NUM_THREADS` to the number of CPUs, which is what bounds Lake 5's
concurrency (it has no jobs flag). `--mem-gb` is enforced with a transient
`systemd-run --user --scope` when available; the same scope accounts for the whole
process tree and is stopped after each build, so stragglers are counted and
killed. Where a cap cannot be enforced the harness says so loudly; an
unenforced CPU cap makes the run non-standard, an unenforced memory cap is
recorded in the result. All of this goes into the result file along with a
machine fingerprint. Only the ratios between tools measured in the same run are comparable
across machines.

The environment is scrubbed: every `LAKE_*`, `LEAN_*` (except the `LEAN_CC` the
harness sets itself), `ELAN_TOOLCHAIN`, `XDG_CACHE_HOME`, `TMPDIR`, `CC`, `AR`, and
`CURL` variable is dropped, and each tool gets private, empty `LAKE_CACHE_DIR`,
`LAKE_CONFIG`, `XDG_CACHE_HOME`, and `TMPDIR` directories, so no state survives
between tools or runs. A cache that persists across runs would therefore have to
be an explicit, separately benchmarked feature.

## Correctness

After **every** build the harness hashes each `.olean`, `.ilean`, and `.c` under
`.lake/build` and compares them with stock Lake's at the same step; it also
requires every build to succeed, no processes to be left behind, and the
breaking-edit build to fail with the same number of failed jobs as Lake's. Any
difference marks the candidate incorrect. (`.trace`, `.hash`, and `.setup.json`
files are expected to differ; they contain absolute paths and tool-specific data.)
A candidate that skips a rebuild whose output would have been identical is
correct, which is deliberate: that is a real optimization.

## Standard runs and the verdict

A run is **standard** when it uses the default corpora, tools, and scenarios,
`--repeat` of at least 3 and `--repeat-fast` of at least 10, no `--main-ref`, a
clean git tree (changes under `research/` are ignored), and an enforced CPU cap. Standard runs
are written to `research/results/` and are the only ones the leaderboard counts;
everything else goes to `bench/work/results/` (or `--out`).

The verdict compares the candidate with `main` per corpus and scenario on
**paired runs**: because the tools are interleaved, repetition *i* of the candidate
and repetition *i* of `main` ran within seconds of each other, so their ratio
cancels the machine's drift between speed states (the same binary alternates
between modes ~20% apart over seconds, which defeats medians, spreads, and
minima alike). The speedup is the median of those ratios. A scenario counts as
fast when every tool's first run took under 2 s; fast scenarios get 10
repetitions and a 7% threshold, others 3 repetitions and 3%, `cold` (one run) 7%.
The fast/slow decision is stored in the result and reused by `--rescore`. Improved at `≥ 1 + t`, regressed at `≤ 1 − t`. Two builds of identical
source stayed within 5.5% under this statistic on the reference machine; see
`research/log/2026-10-29-baseline.md`. `--rescore FILE` recomputes a stored
result's verdict under the current rule and records that it did. Overall: `failure` if any correctness
check failed or anything regressed; `success` if a primary scenario improved and
nothing regressed; otherwise `neutral`. The harness exits with status 2 on
`failure`.

## Running pieces by hand

```sh
python3 bench/harness/run.py --quick --tools lake,candidate      # 3 small corpora, ~3 min
python3 bench/harness/run.py --configs medium-deep --scenarios cold,noop --repeat 5
python3 bench/harness/run.py --keep                              # keep bench/work/corpora/*/<tool>/ for experiments
python3 bench/gen/generate.py --shape wide --modules 50 --out /tmp/c && cd /tmp/c && lake build
```

Changing the generator or the configs changes every future comparison. Such changes
go in their own PR, discussed first, and bump `GENERATOR_VERSION` so stale corpora
are regenerated.
