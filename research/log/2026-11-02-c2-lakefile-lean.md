# c2-lakefile-lean: read `lakefile.lean` through Lake's own loader (C2)

- **Date:** 2026-11-02
- **Author:** Claude (Opus 5.5), owner loop, machine `2e698309` (i9-14900K, 16 of 32 CPUs, 32 GB cap)
- **Milestone:** C2 (`lakefile.lean`)
- **Outcome:** failure (harness verdict `failure` twice, each on one noise-level `noop` regression on a different corpus; correct throughout; direct measurement shows no cost)
- **Branch / PR:** code on `research/c2-lakefile-lean-wip` (draft PR https://github.com/adamtopaz/flume/pull/7, closed); this entry: `research/c2-lakefile-lean-log`
- **Result files:** research/results/20261103T023000Z-2e698309-14d8e16.json, research/results/20261103T033800Z-2e698309-f659352.json (same code)

## Plan

Most real Lake projects (Mathlib and everything downstream of it) are configured in
`lakefile.lean`, which flume refuses today. A Lean configuration is a Lean program, so only
Lake can evaluate it faithfully. The roadmap's sketch is a worker that loads it with Lake
as a library and prints JSON; Lake already ships exactly that as a command:
`lake translate-config toml <out>` loads the root package with Lake's own loader
(`loadPackage`, no dependency resolution) and writes the evaluated, declarative
configuration as TOML (`Package.mkConfigString`), which flume already reads. So:

- `config::load` follows Lake's choice of file (`Load/Package.lean`: `lakefile.lean` wins
  over `lakefile.toml` when both exist; flume read the TOML file then, a silent mismatch);
- for `lakefile.lean`, flume runs the toolchain's `lake translate-config toml` once and
  caches the TOML in `.lake/flume/config/` under a key of the configuration's content, the
  local files it imports, and the toolchain's githash; a no-op build only hashes the file;
- Lake's translation is lossy by design ("non-declarative configuration will be
  discarded"): custom `target`s, `script`s, facets, `extern_lib`s, and `post_update` hooks
  vanish. flume scans the configuration (comments and strings removed) for those commands
  and refuses it, naming the construct, so nothing is ignored silently (DESIGN.md). What
  TOML can express but flume does not support (`require`, `lean_exe`, ...) is already
  refused by the TOML reader.

How I will know: `tests/check_lakefile_lean.py` converts the small benchmark corpora and a
hand-written multi-library configuration to `lakefile.lean`, builds each with stock Lake
and with flume, and requires identical artifacts; it also checks caching, re-translation
after an edit, the file precedence, and the refusals. The standard suite must stay correct
and not slower (`milestone:`; the corpora use `lakefile.toml`). Lake's
`tests/lake/examples` (bootstrap, deps, ffi, hello, precompile, reverse-ffi, scripts) all
need executables, dependencies, native code, or scripts, so none qualifies as
"only build libraries" yet; the done-when's second half is vacuous until C1/C4.

## What was done

- `crates/flume/src/lean_config.rs` (new): `translate` runs the toolchain's
  `lake translate-config toml <tmp>` in the package root (Lake refuses to overwrite, so
  the output goes to a fresh name), and caches the TOML in
  `.lake/flume/config/<key>.toml`, keeping only the newest. The key hashes a version tag,
  the toolchain's githash, `lakefile.lean` with line endings normalized, and every local
  file it imports (its header parsed with flume's own parser; `Lake`, `Lean`, `Init`, and
  `Std` imports are the toolchain's). Before that, `discarded_command` scans the file,
  with comments (`--`, nested `/- -/`) and strings (`"..."`, `r#"..."#`) blanked out, for a
  line whose first word after attributes is `target`, `script`, `extern_lib`,
  `module_facet`, `library_facet`, `package_facet`, or `post_update` followed by a name
  (so `target := 1` or `target.foo` in a definition do not count); flume then refuses the
  configuration and names the construct. Unit tests cover the scanner.
- `crates/flume/src/config.rs`: `load` picks `lakefile.lean` over `lakefile.toml` (Lake's
  order) and parses either text with the same TOML reader, so every rule for unsupported
  keys (dependencies, executables, unknown options) applies to Lean configurations too.
- `tests/check_lakefile_lean.py` (new): the four small benchmark corpora rewritten as
  `lakefile.lean` (by `lake translate-config lean`) and a hand-written configuration (two
  libraries, `srcDir`, package and library `leanOptions`, `moreLeanArgs`, `weakLeanArgs`
  computed by a helper `def` and `s!`, a library inside `meta if`), each built by stock
  Lake and by flume; then caching, an edit, precedence over `lakefile.toml`, and refusals
  of `script`, `target`, and `require`.
- `research/DESIGN.md`: the configuration path, the diagram, and the file list.

Checks: `cargo build/test/clippy/fmt` clean (16 unit tests); `tests/check_lakefile_lean.py`:
identical artifacts for small-deep (1,400 files), small-layered (1,400), small-legacy (600),
small-wide (1,400) and the custom package (12), the translation cached (a no-op build of the
custom package in 3 ms, the cache file untouched), an edit re-read (the two modules of the
library whose arguments changed rebuilt, as Lake did), `lakefile.lean` preferred, and all
three refusals; the first translation of a corpus takes ~1.3 s (Lake elaborating the
configuration).

## Measurements

Quick suite and held-out quick suite (commit `0dfdfa2`): both `correctness of candidate:
identical artifacts to lake, all builds ok, break check ok`.

### Run 1: standard suite, no `--targets` (a compatibility milestone), 5/20 repetitions

`research/results/20261103T023000Z-2e698309-14d8e16.json` (79 min):

```
speedup of candidate vs main: cold=0.993x, noop=1.007x, leaf=0.994x, newfile=1.035x, hub-private=1.010x, hub-interface=1.001x, swarm=0.992x, clone=1.052x, agents-4=0.995x, workflow:single=1.006x, workflow:many=1.023x, workflow:cold=0.993x, overall=1.007x
speedup of candidate vs lake: cold=1.084x, noop=4.136x, leaf=1.451x, newfile=1.761x, hub-private=1.439x, hub-interface=1.092x, swarm=1.218x, clone=2.013x, agents-4=1.325x, workflow:single=1.651x, workflow:many=1.633x, workflow:cold=1.084x, overall=1.430x
correctness of candidate: identical artifacts to lake, all builds ok, break check ok
verdict medium-deep/noop: improved (paired speedup 1.074x over 20 runs, threshold 7%; medians main 0.05s, candidate 0.04s)
verdict medium-layered/noop: regressed (paired speedup 0.922x over 20 runs, threshold 7%; medians main 0.04s, candidate 0.05s)
verdict medium-layered/newfile: improved (paired speedup 1.082x over 20 runs, threshold 7%; medians main 0.24s, candidate 0.22s)
verdict medium-layered/clone: regressed (paired speedup 0.900x over 20 runs, threshold 7%; medians main 0.20s, candidate 0.19s)
verdict: failure
```

(The other 27 comparisons: `same`.) For a `lakefile.toml` project the only new work is one
`stat` of `lakefile.lean` before reading the TOML, so a 4 ms (`noop`) or 20 ms (`clone`)
cost is not possible; the pairs say the same: per-pair ratios of medium-layered/clone range
from 0.43 to 1.77 and its candidate median (0.19 s) is *below* `main`'s (0.20 s), the noop
pairs range from 0.62 to 1.37, and the same scenario on medium-deep "improved" by 7.4%.
The 15-minute load average was 22 at the end of the run (up to 16 of it the run itself).
Reran with the same settings, as instructions.md says for noise.

### Run 2: the same

`research/results/20261103T033800Z-2e698309-f659352.json` (80 min):

```
speedup of candidate vs main: cold=1.042x, noop=1.006x, leaf=1.004x, newfile=0.999x, hub-private=0.997x, hub-interface=0.998x, swarm=1.014x, clone=1.088x, agents-4=0.994x, workflow:single=1.003x, workflow:many=1.040x, workflow:cold=1.042x, overall=1.028x
speedup of candidate vs lake: cold=1.131x, noop=4.168x, leaf=1.392x, newfile=1.923x, hub-private=1.483x, hub-interface=1.111x, swarm=1.226x, clone=1.794x, agents-4=1.391x, workflow:single=1.681x, workflow:many=1.580x, workflow:cold=1.131x, overall=1.443x
correctness of candidate: identical artifacts to lake, all builds ok, break check ok
verdict medium-deep/noop: regressed (paired speedup 0.927x over 20 runs, threshold 7%; medians main 0.04s, candidate 0.04s)
verdict medium-deep/clone: improved (paired speedup 1.115x over 20 runs, threshold 7%; medians main 0.19s, candidate 0.16s)
verdict: failure
```

(The other 29: `same`; run 1's medium-layered flags are gone.) `noop` regressed in both
runs, but on a different corpus each time, and in run 1 the corpus that regresses now
"improved" 7.4%. Since one scenario was flagged twice, I measured it directly, outside the
harness's per-build systemd scope: 300 interleaved no-op builds of medium-deep (1,000
modules) with `main`'s binary (`2247677`) and the candidate's, on the same checkout:

```
main: median 15.09 ms, p10 12.98, p90 17.98
candidate: median 15.10 ms, p10 12.74, p90 17.66
paired median main/candidate: 1.003
```

No difference. The harness's `noop` (~40 ms) is mostly the scope's start and stop, whose
spread (0.03-0.06 s across the 20 runs of either tool) is far larger than the 7%
threshold.

## Conclusion

- **C2 works and is correct**, but did not get a passing verdict in this iteration:
  - flume now builds `lakefile.lean` projects by having Lake translate the configuration,
    cached per content;
  - the small benchmark corpora as `lakefile.lean` and a hand-written multi-library
    configuration build with artifacts identical to stock Lake's;
  - declarations Lake's translation would drop are refused, and `lakefile.lean` wins over
    `lakefile.toml` as in Lake;
  - on the standard suite (whose corpora use `lakefile.toml`) everything was correct, and
    two runs each flagged one `noop` "regression" that direct measurement shows is not
    there.
- **Not merged because of the time budget:** a third standard run plus CI would have ended
  past four hours. Following the rules, the `failure` verdict stands and the code waits on
  `research/c2-lakefile-lean-wip`.
- **Next iteration: re-measure `research/c2-lakefile-lean-wip` unchanged** (merge `main`
  first if it moved, then the standard suite with `--repeat 5 --repeat-fast 20`). This is
  the same situation as `2026-11-02-p2-local-cache.md`, whose re-measurement passed. Start
  it early in the iteration, so that a run that needs repeating fits in the budget.
- **For the harness owner:** on this machine `noop` (and `clone`) under 0.2 s carry the
  systemd scope's noise, ±30% per run. Three of the last six standard runs failed on such a
  scenario while the code under test could not affect it. A `bench:` change could subtract
  an empty scope's cost, or use the median of more runs for scenarios under 0.1 s.
