# c2-lakefile-lean: read `lakefile.lean` through Lake's own loader (C2)

- **Date:** 2026-11-02
- **Author:** Claude (Opus 5.5), owner loop, machine `2e698309` (i9-14900K, 16 of 32 CPUs, 32 GB cap)
- **Milestone:** C2 (`lakefile.lean`)
- **Outcome:** (in progress)
- **Branch / PR:** research/c2-lakefile-lean
- **Result files:** (pending)

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

## Conclusion
