# flume: design

flume is a build system for Lean 4 written to be fast first and small enough to
change quickly. It replaces Lake's role of deciding what to build and running the
builds, and it reuses the stock Lean toolchain for everything that needs Lean's
semantics. This document is the contract every iteration builds on. Change it in
the same PR as the code that changes the architecture.

## Goals, in order

1. **Correct.** For the same inputs flume writes `.olean`, `.ilean`, `.c`, and the
   module-system artifacts (`.olean.server`, `.olean.private`, `.ir`, `.ir.sig`)
   byte-identical to stock Lake's, after cold and incremental builds, and fails
   exactly where Lake fails. The benchmark harness checks this after every build.
2. **Fast**, on three workflows weighted equally:
   - single checkout, an edit-build loop (no-op, leaf, new file, hub edits that do
     and do not change the interface, many little edits at once);
   - many agents, each in its own checkout of one project on one machine;
   - a fresh clone or CI building everything.

   Builds across several machines come later.
3. **Compatible.** Every Lake project builds unchanged (`lakefile.toml` and
   `lakefile.lean`, git dependencies, executables and native code), and editors work
   on flume's builds. Version 0 covers only part of this; the roadmap closes the gap.

## Architecture

```
            flume (Rust)                                stock Lean toolchain
 ┌───────────────────────────────────┐        ┌────────────────────────────────┐
 │ config: lakefile.toml  (v0)        │        │ lean      compiles one module  │
 │         lakefile.lean ──────────────┼─JSON──▶│ flume-worker (Lean, links Lean │
 │ graph:  headers parsed in Rust     │        │   and Lake as libraries): Lean's│
 │         (Lean's grammar, checked)  │        │   header parser, later config   │
 │ keys:   Lake's trace rules, hashed │        │   evaluation and elaboration    │
 │ store:  .lake/flume/state.json     │        └────────────────────────────────┘
 │ cache:  ~/.cache/flume, by key and │
 │         content; shared by clones  │
 │ sched:  N workers, tallest first   │──spawn `lean` with Lake's exact arguments──▶
 └───────────────────────────────────┘
```

- **The driver** (`crates/flume`, Rust) owns everything that does not need Lean's
  semantics: reading configurations, parsing headers, the module graph, deciding
  what is up to date, scheduling, and running compilations. It starts in about a
  millisecond and does a no-op build of 1,000 modules in a few milliseconds.
- **The worker** (`worker/`, Lean) is a small program linked against the stock
  toolchain's libraries, including Lake's. It does what only Lean can do. Version 0
  exposes Lean's own header parser, the oracle for the driver's
  (`tests/check_headers.py`). Next it evaluates `lakefile.lean` configurations with
  Lake's own loader, and later it may elaborate modules itself (persistent process,
  environment reuse) where that pays off and stays byte-identical.
- **Compilation** runs the stock `lean` exactly as Lake 5 does: `lean <weak and more
  args> <file> -o <olean> -i <ilean> -c <c> --setup <file>.setup.json --json`, with
  `LEAN_PATH` set to the package's library directory and a setup file listing the
  same import artifacts Lake lists. Outputs go where Lake puts them
  (`.lake/build/lib/lean`, `.lake/build/ir`), so tools that read Lake's layout work.
- **The shared cache** (`cache.rs`, roadmap P2) plays the part of Lake's artifact cache
  for every checkout on the machine: `$FLUME_CACHE_DIR`, else `$XDG_CACHE_HOME/flume`,
  else `~/.cache/flume` (`FLUME_CACHE=off` disables it). Artifacts are stored once by
  content (`art/`), and each successful compile's outputs and messages are recorded
  under the module's key (`key/<os>-<arch>/`). A module that is not up to date but whose
  key is recorded gets its outputs hard-linked (or copied) into the build directory
  instead of compiled, and its warnings replayed; `clone` and the repair after a
  breaking edit cost links, not compiles. There is no size bound yet (P2's next step).

## The rules that keep flume correct

- **Keys follow Lake's traces.** A module's key hashes the toolchain githash, the
  package name, the library's options and arguments, the module name and source, and
  for each local import exactly the artifact and transitive traces Lake mixes in
  (`ModuleImportInfo.addImport`, `Module.computeExportInfo` in Lake 5): a
  module-system importer depends on an import's `.olean` and on what that import
  re-exports, `meta import` adds the IR, `import all` adds everything, and a
  non-module importer depends on all artifacts below it. flume therefore rebuilds a
  module exactly when Lake would. An iteration that rebuilds less than Lake must
  prove that the skipped outputs are byte-identical, and the harness's identity
  check must be able to catch it if they are not.
- **Setup files follow Lake's import walk** (`fetchTransImportArts`), including
  `import all` and `meta import` widening.
- **Old outputs are removed before a module is compiled**, as Lake does, so a failed
  compile never leaves stale artifacts. A compile fails when `lean` exits non-zero or
  logs an error.
- **Imports resolve as in Lake** (`Package.findModule?`: the last library whose globs or
  roots make the module buildable); other imports are left to `lean`, which rejects an
  unknown one. Library globs expand as Lake's do (symlinks followed; a missing base file
  fails its compile; a missing directory fails that library's target only).
- **Failures stay local, as in Lake.** A module whose header cannot be read or parsed, or
  that lies on an import cycle, fails by itself and blocks only its dependents. Which
  modules get built, the artifacts, and the exit status match Lake; the wording and the
  number of `✖` lines for such header-level problems do not (Lake reports them as
  separate jobs).
- **Nothing is silently ignored.** A configuration option flume does not implement
  is an error, not a no-op.
- **Up-to-date checks trust file status, then content.** An unchanged source (size,
  mtime, ctime, inode) is not reread, unless it was modified within two seconds of being
  read (timestamps that coarse cannot be trusted). Sources are hashed with line endings
  normalized, as Lake's `Hash.ofText` does. Every output must exist; one whose status
  changed is rehashed and accepted only if its content matches what was built, which
  keeps copied checkouts cheap and catches deleted or replaced files.
- **The cache is keyed like the state.** A cache entry is used only for the exact key a
  compile would have been checked against, and the key contains no paths, so a restored
  module's outputs are the ones Lake would accept for the same traces (Lake's own cache
  makes the same inference). Cached files and restored outputs are read-only, as in
  Lake, because they are hard links; files appear atomically (link or rename), so builds
  in several checkouts may share the cache; an artifact that is missing or has the wrong
  size makes the module compile, and that compile repairs the cache.
- **One build per checkout at a time** (a lock in `.lake/flume/`), and a panic while
  processing a module fails that module only.

## Output and exit status

flume prints Lake's progress format (`✔ [i/n] Built M (t)`, `⚠` with warnings,
`✖ [i/n] Building M` with the errors), ends with `Build completed successfully (N
jobs).` or `error: build failed`, and exits 0 or 1. The harness counts failed jobs
from the `✖` lines and compares them with Lake's.

## Principles for changes

- **Measure first.** Profile the scenario you want to speed up (`perf`, `strace`,
  `lean --profile`, flume's own timing) before changing anything.
- **Keep flume small.** A dependency or abstraction has to pay for itself in speed
  or in compatibility. `cargo build --release` of the driver takes seconds; keep it so.
- **Stock Lean.** flume uses the release pinned in `lean-toolchain` as a library and
  never patches it. If a milestone shows that a speedup needs a change to Lean, the
  roadmap gets an entry for a small patch series first, and the maintainer decides.
- **Content addresses everywhere.** State, caches, and keys are hashes of content,
  so copies, clones, and other machines can share them safely.

## Where things are

- `crates/flume/src/main.rs`: command line (`build`, `query`, `clean`, `debug-headers`).
- `crates/flume/src/config.rs`: `lakefile.toml`, libraries, globs.
- `crates/flume/src/header.rs`: Lean's header grammar (port of `ParseImportsFast.lean`).
- `crates/flume/src/toolchain.rs`: finding the toolchain without starting `lean`.
- `crates/flume/src/build.rs`: discovery, keys, setup files, scheduler, compilation.
- `crates/flume/src/store.rs`: what is remembered between builds.
- `crates/flume/src/cache.rs`: the artifact cache shared by checkouts.
- `worker/`: the Lean worker. `tests/`: checks that need a built toolchain
  (`check_headers.py`, `check_cache.py`).
- `bench/`: corpus generator, harness, configs. `research/`: this file, the roadmap,
  the log of every attempt, idea write-ups, and results.
