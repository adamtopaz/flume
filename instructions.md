# flume research loop: instructions for coding agents

You are about to run **one iteration** of a crowd-sourced research loop whose purpose
is to make **flume**, a minimal build system for Lean 4, build gigantic Lean projects
far faster than Lake while staying exactly as correct. Anyone can run an iteration on
their own machine by pointing a coding agent at this file. One iteration takes about
**four hours** of wall-clock time and ends with a pull request.

Read this whole file, then `research/DESIGN.md` and `research/ROADMAP.md`, before
doing anything. The rules below are not optional; a PR that breaks one is not merged.

## 0. The goal

Axiom's formalization of the Classification of Finite Simple Groups grew to tens of
millions of lines; a cold build with Lake of an early 15-million-line version took 11
hours on 32 cores, and Axiom's own tool builds 50 million lines in 2 hours. flume should
beat that, one measured step at a time.

flume is a Rust driver plus a small Lean worker that use the stock Lean toolchain as a
library (`research/DESIGN.md`). Progress is organized as milestones
(`research/ROADMAP.md`): **compatibility** milestones make more Lake projects build
(dependencies, `lakefile.lean`, editors, native code) and **performance** milestones
make builds faster on three equally weighted workflows: edits in one checkout, many
agents in many checkouts, and fresh-clone cold builds.

**What may not change.**

- **Artifacts.** For the same inputs flume writes `.olean`, `.ilean`, `.c`, `.olean.server`,
  `.olean.private`, `.ir`, and `.ir.sig` files **byte-identical** to stock Lake's, after
  cold *and* incremental builds, and fails exactly where Lake fails. The harness checks
  this after every build. A candidate that fails any check is not an improvement,
  however fast.
- **Lean.** flume uses the release pinned in `lean-toolchain` and never patches it
  (see "Stock Lean" in `research/DESIGN.md`). Do not change `lean-toolchain`.
- **Lake's semantics.** flume rebuilds a module exactly when Lake would (its keys follow
  Lake's traces). Rebuilding less needs a proof that the skipped outputs are identical
  and a test that would catch it if they were not.
- **Honesty of the measurement.** flume may not behave differently because it is being
  benchmarked: no code may depend on the corpus's names (`Corpus.*`, `bench_*`), its
  paths, the harness's environment variables, or its edit pattern. No machine-specific
  build flags (such as `-C target-cpu=native`). Parallelism must respect the caps
  (`LEAN_NUM_THREADS` and the CPU set). A process flume starts must not outlive the
  command, except a build server started through the server protocol
  (`research/ROADMAP.md`, P3), which counts against the same caps.

## 1. Set up (about 15 minutes)

Prerequisites: `git`, `gh` (for the PR), and either Nix with flakes (recommended:
`nix develop` provides Rust, elan, Python, and the rest) or Rust 1.85+ (`rustup`),
[`elan`](https://github.com/leanprover/elan), and Python 3.10+. Linux is the reference
platform; CPU and memory caps are enforced only there (CPU affinity and `systemd-run
--user`). On macOS results are marked non-standard, so a macOS contributor can submit
`log:` PRs but not the standard results `research:` and `milestone:` PRs need.

```sh
git clone https://github.com/adamtopaz/flume && cd flume
gh repo fork --remote --remote-name fork      # origin stays upstream; `fork` is yours
nix develop                                    # or have cargo, elan, python3 on PATH
cargo build --release                          # target/release/flume (~30 s first time)
cargo test --release
(cd worker && lake build)                      # the Lean worker (~10 s)
python3 tests/check_headers.py                 # flume's header parser vs Lean's (~2,500 files)
```

**Caps.** Benchmarks must not take over the machine. Decide what this machine may
spend, derive it from `nproc` and your RAM (do not copy the example), and write it to
`bench/local.json` (gitignored). The default is half of the machine.

```json
{"cpus": "0-15", "mem_gb": 32}
```

Then run the harness's smoke test (it must report the candidate correct):

```sh
python3 bench/harness/run.py --suite quick --tools lake,candidate   # ~10 min on 16 cores
```

It must print `correctness of candidate: identical artifacts to lake, all builds ok,
break check ok`.

## 2. Orient (about 15 minutes)

1. Read `research/ROADMAP.md` and pick the milestone (or the next step of one) you will
   work on: open, with its dependencies done, not claimed by an open PR
   (`gh pr list --repo adamtopaz/flume`).
2. Read `research/ideas/` (what worked) and `research/log/` (every attempt, including
   failures). Do not repeat a failed idea without a genuinely new angle, and say so.
3. Read `bench/README.md`, and `research/results/LEADERBOARD.md` once results exist (a
   workflow generates it after the first results are merged).
4. **Profile before guessing.** `python3 bench/harness/run.py --suite quick --keep`
   leaves per-tool corpus copies under `bench/work/corpora/`; `flume -v build` prints
   every message; `perf` and `strace -c` work on flume and on `lean`.

## 3. Plan (about 15 minutes)

1. Branch from the latest main: `git fetch origin && git switch -c research/<slug> origin/main`.
   If `origin/main` moves, merge it in; do not rebase (results name the commits they
   measured).
2. Claim the milestone: push the branch and open a draft PR titled with the milestone id
   (`gh pr create --draft ...`).
3. Create your log entry: `cp research/log/TEMPLATE.md research/log/$(date +%F)-<slug>.md`
   and fill in what you will do, why, and how you will know it worked. The entry exists
   so that even an abandoned attempt leaves knowledge behind.

## 4. Implement (at most 2 hours)

- Keep the change focused on the milestone step. Prefer small, measured steps.
- Add tests: Rust unit tests for logic, `tests/` scripts for anything needing Lean.
  Compatibility milestones need a benchmark corpus or test project that exercises the new
  feature; benchmark changes go in a separate `bench:` PR first (rule 6 below).
- Keep notes in your log entry as you go.
- **Commit before you measure.** Results record the commit they measured.
- If it is not working after two hours, stop, record what you learned, and go to step 7
  with outcome `inconclusive`.

## 5. Verify (about 15 minutes)

All of these must pass before measuring:

```sh
cargo build --release && cargo test --release && cargo clippy --release -- -D warnings && cargo fmt --check
python3 tests/check_headers.py
python3 bench/harness/run.py --suite quick --tools lake,candidate
```

If the harness prints `INCORRECT`, your change is wrong, not clever.

## 6. Measure (about 50 minutes on 16 cores; longer on fewer)

Run the standard suite detached and poll its log; it measures stock Lake, flume `main`
(the merge-base of your branch and `origin/main`), and your candidate on the same
corpora, interleaved, under the same caps:

```sh
nohup python3 bench/harness/run.py --label <slug> --targets <scenarios> > bench/work/run.log 2>&1 &
tail -f bench/work/run.log      # it ends with `wrote ...`
```

`--targets` names the milestone's target scenarios (from the roadmap); omit it for a
compatibility milestone. Performance milestones that say "milestone suite" add
`--suite milestone` (hours; it also measures Blake). The harness prints a table, speedup
lines per scenario and per workflow, one `verdict` line per corpus and scenario, and a
final `verdict: success|neutral|failure`, and writes `research/results/<file>.json`.

The rule: per corpus and scenario, repetition *i* of the candidate is compared with
repetition *i* of `main` and the speedup is the median ratio. Scenarios under 2 s get 10
repetitions and a 7% threshold, longer ones 3 and 3%, `cold` one and 7%. **success**: a
target scenario improved on some corpus and nothing regressed anywhere, all correctness
checks passing; **failure**: anything regressed or any check failed; otherwise
**neutral**. If a result looks like noise, rerun with `--repeat 5 --repeat-fast 20`.
**Commit every result file the standard suite writes**, including the ones you do not
like. Never tune the benchmark, corpora, or caps to make a result look better.

## 7. Record (about 15 minutes)

Finish your log entry: outcome, the harness's verdict and speedup lines verbatim, result
file names, and what the next person should know. Update the milestone's status in
`research/ROADMAP.md`. For a successful performance milestone, also write
`research/ideas/<slug>.md` from its template.

- `research:` PR (performance, verdict `success`): code, tests, log entry, idea write-up,
  results, roadmap update.
- `milestone:` PR (compatibility or infrastructure, verdict not `failure`, all checks
  correct): code, tests, log entry, results, roadmap update.
- `log:` PR (failed or inconclusive): only the log entry and results, on a branch without
  the code (push the code as `research/<slug>-wip` on your fork and link it).

## 8. Submit

```sh
python3 ci/pr_check.py --base origin/main --title "<prefix>: <one line>"
git push -u fork research/<slug>
gh pr create --repo adamtopaz/flume --base main --head <you>:research/<slug> \
  --title "<prefix>: <one line>" --body-file research/log/<your-entry>.md
```

Mark your draft ready for review. CI builds flume and the worker, runs the tests and the
header check, and runs the quick suite for correctness; it never measures performance
(your result file is the measurement). The PR check judges your results against the code
as it would be merged: if `main`'s code changed since you measured, merge `main` into
your branch and measure again. Fix red checks. The maintainer merges with merge
commits only, so every measured commit stays reachable.

## Hard rules, in one place

1. One milestone step per iteration; about four hours; always end with a PR.
2. Artifacts byte-identical to Lake's after every build; failures match Lake's.
3. Stock Lean only; `lean-toolchain` unchanged.
4. No benchmark-aware behavior, no machine-specific flags, no ignoring the caps, no
   processes outliving the command outside the server protocol.
5. Commit before measuring; measure with the standard suite; commit every result it
   writes; let the harness decide.
6. Never change the benchmark (generator, harness, configs) in the same PR as flume's code.
7. Record every attempt, including failures, in `research/log/`.
