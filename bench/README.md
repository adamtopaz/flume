# flume benchmarks

Everything here answers one question reproducibly: *is this version of flume faster
than the last one, and than stock Lake, on builds that look like a gigantic Lean
project, while producing exactly Lake's artifacts?* See `../instructions.md` for how the
research loop uses it.

## Pieces

- `gen/generate.py`: generates a self-contained Lean package (imports only Lean core)
  with a controlled import graph and per-module elaboration cost. Deterministic: the
  same config always yields byte-identical files. By default every file is a Lean
  module (`module`, `public import`, a `public section`, `@[expose]` definitions, some
  `private` declarations); `module_system: false` writes ordinary files. Statistics
  and scenario targets are computed in near-linear time, so 100,000 modules generate
  in about 15 seconds.
- `configs/*.json`: the corpora. Three graph shapes (`layered`: random layered DAG;
  `deep`: a few long chains, long critical path; `wide`: hubs imported by nearly
  everything) at four sizes: `small` (200 modules), `medium` (1,000), `large` (10,000),
  `huge` (100,000); plus `small-legacy` and `medium-legacy`, ordinary-file corpora that
  guard projects outside the module system. A config may restrict its scenarios
  (`scenarios`, `exclude_scenarios`).
- `harness/run.py`: the measurement. Builds the tools, copies each corpus once per
  tool, runs the scenarios interleaved, checks correctness after every build, decides
  the verdict, and writes one JSON result. `harness/edits.py` defines the edits;
  `harness/multi_build.py` runs concurrent agent builds in one scope;
  `harness/scope_stats.sh` records each scope's memory and CPU;
  `harness/leaderboard.py` renders `../research/results/LEADERBOARD.md`.
- `blake.json`: the Blake commit milestone runs compare against.
- `local.json` (gitignored): this machine's caps, e.g. `{"cpus": "0-15", "mem_gb": 32}`.
- `work/` (gitignored): corpora, per-tool copies and caches, the `main` worktree, Blake's
  checkout, logs, and results of non-standard runs.

## Tools

| tool | what it is |
|---|---|
| `lake` | the stock toolchain's `lake` (the release in `../lean-toolchain`) |
| `main` | flume at the merge-base of `HEAD` and `origin/main`, built with `cargo build --release` |
| `candidate` | flume in this checkout |
| `blake` | Blake (github.com/adamtopaz/blake) at the commit in `blake.json`; milestone suite only |

Every tool runs `<exe> build` in its copy of the corpus with stock Lean. Lake and Blake
run with Lake's artifact cache enabled (`LAKE_ARTIFACT_CACHE=true`) in a private
directory per tool and corpus, shared by that tool's checkouts, as an agent swarm would
configure them; remote cache downloads are off (`LAKE_NO_CACHE=1`).

## Suites

| suite | corpora | repetitions | use |
|---|---|---|---|
| `quick` | small-* | 1 / 2 | smoke test, CI (never standard) |
| `standard` (default) | medium-* | 3 / 10 | every iteration, ~50 min on 16 cores |
| `milestone` | medium-* and large-layered | 3 / 10 | performance milestones; adds `agents-16` and Blake; hours |
| `release` | huge-* | 1 / 5 | rarely; tools run one after another (disk), cheap scenarios only |

## Scenarios

| workflow | scenario | what happens |
|---|---|---|
| cold | `cold` | first build of a fresh checkout, empty caches |
| single checkout | `noop` | build with nothing changed |
| | `leaf` | edit the deepest module nobody imports (the edit kind rotates) |
| | `newfile` | add a module importing the leaf and the hub |
| | `hub-private` | edit the most widely imported module without changing its interface (a proof, a private theorem) |
| | `hub-interface` | change the hub's interface (a public theorem, a statement, an exposed body) |
| | `swarm` | edit up to 24 little-imported modules at once, mixed kinds |
| many checkouts | `clone` | a fresh checkout of the current sources while the tool's cache holds everything |
| | `agents-4`, `agents-16` | 4 or 16 built checkouts each edit their own module and build at once; time until all finish |
| (untimed) | `break` | two breaking edits to the hub must fail with as many failed jobs as Lake, then recover |

Edit kinds (`harness/edits.py`): `proof` and `private` keep a module's interface;
`public`, `statement`, and `body` change it (still building); every edit is unique and
applied identically to every tool's copy.

A scenario whose first run takes under 2 s gets the suite's fast repetition count and a
7% threshold; longer ones 3% (`cold`: one run, 7%). The tools are **interleaved**: each
repetition applies the same edit to every copy and builds the tools in a rotating
order, so drift of the machine affects all tools alike; verdicts compare repetition *i*
of the candidate with repetition *i* of `main`.

## Caps and comparability

The harness pins itself and every child to `--cpus` (Linux) and sets
`LEAN_NUM_THREADS` to their count; `--mem-gb` is enforced with a transient `systemd-run
--user --scope` per build (or per group of agent builds), which also accounts for the
whole process tree and finds stragglers. An unenforced CPU cap makes a run
non-standard. The environment is scrubbed of `LAKE_*`, `LEAN_*`, `ELAN_TOOLCHAIN`, and
cache variables; each tool gets private cache and temp directories.

## Correctness

After **every** build the harness hashes every artifact of every module (`.olean`,
`.ilean`, `.c`, and for module-system corpora `.olean.server`, `.olean.private`, `.ir`,
`.ir.sig`) and compares them with stock Lake's at the same step. Artifacts are read
from `.lake/build`; one that is not there is located with the tool's `query` command.
It also requires every build to succeed, no processes to be left behind, and both
breaking edits to fail with the same number of failed jobs as Lake. Any difference marks
the candidate incorrect.

## Build servers

A tool may keep a server running between builds. It advertises one by printing
`flume-server 1` for `<exe> server protocol`; the harness starts it with `<exe> server
start` in its own scope (same caps), measures the single-checkout and many-checkouts
scenarios again with the server warm (keys get `@warm`), and stops it with `<exe> server
stop`, after which no process may remain. No tool has a server yet (roadmap P3).
