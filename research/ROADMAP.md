# flume: roadmap

Each milestone is one or a few research-loop iterations (about four hours each; see
`../instructions.md`). Pick an **open** milestone whose dependencies are **done**,
claim it by opening a draft PR whose title names it (e.g. `milestone: C1 git
dependencies (draft)`), and update its status here in the PR that finishes it.

- **Compatibility** milestones (C) make more projects build. They merge as
  `milestone:` PRs: correct, no scenario slower than `main`, plus tests.
- **Performance** milestones (P) make flume faster. They merge as `research:` PRs:
  the standard suite (or the milestone suite where stated) run with `--targets` set
  to the milestone's target scenarios must say `success`: a target faster beyond
  noise, nothing slower.

A milestone may be split into steps; record the split here. Statuses: open, claimed
(with the PR), done (with the PR).

## Done

| id | milestone | done in |
|---|---|---|
| M0 | **flume v0.** Builds `lakefile.toml` library packages without dependencies: Lake's exact `lean` invocations, setup files, and rebuild rules; byte-identical artifacts on every benchmark corpus and scenario; no-op in milliseconds (stat cache); Lean worker exposing Lean's header parser, checked against the Rust parser on Lean's own 2,500 sources; benchmark, harness, CI, and this loop. | scaffold PR |

## Compatibility

| id | milestone | depends on | done when | status |
|---|---|---|---|---|
| C1 | **Dependencies.** `[[require]]` with path and git sources; read `lake-manifest.json` and fetch git dependencies at the manifest's revisions into `.lake/packages`; build dependencies' libraries with their own options; package-qualified setup (`package` per module, `LEAN_PATH` with every package's library directory). | M0 | A corpus with a path dependency and a git dependency (generated, self-contained) builds with identical artifacts; the harness gains such a corpus in a `bench:` PR first. | open |
| C2 | **`lakefile.lean`.** The worker evaluates a Lean configuration with Lake's own loader (as a library) and emits the package description as JSON; flume caches it by the configuration's content and imports. | M0 | The benchmark corpora written as `lakefile.lean` build identically; Lake's `tests/lake/examples` that only build libraries pass. | open |
| C3 | **Editors.** `lake serve` / `lake setup-file` compatibility: Lean's language server asks the build system for a module's setup (building its imports first); flume answers from its own graph and state. Ship a `lake` shim so editors and other tools find flume. | C2 | VS Code with the Lean extension opens a corpus module, sees diagnostics, and goes to definitions across modules, with flume's builds; a scripted check in `tests/`. | open |
| C4 | **Executables and native code.** `lean_exe`, `extern_lib`, `precompileModules`, `moreLinkArgs`, shared libraries, plugins and dynlibs in setup files. | C1 | Lake's native examples (`tests/lake/examples/ffi`, `precompile`, `reverse-ffi`) build and run with identical artifacts. | open |
| C5 | **Everything else Lake projects use.** Remaining package and library options (`buildType`, `platformIndependent`, `moreServerArgs`, `compiler.postponeCompile` with `leanir`, custom targets in Lean configurations...), driven by a list of real projects' lakefiles. | C2, C4 | Every option Lake documents either works or has a recorded reason. | open |

## Performance

| id | milestone | targets | done when | status |
|---|---|---|---|---|
| P1 | **No `lean` start-up per module.** A fork server: one pre-initialized Lean process forks a child per compilation (Blake's `one-zygote` showed ~25 ms per module and exact artifacts). Implement in the worker (with a small C shim for `fork` if needed). | leaf, newfile, hub-interface, swarm, cold | standard suite `success` on its targets. | open |
| P2 | **Shared content-addressed cache.** Outputs stored by key in `~/.cache/flume` (configurable, size-bounded); a checkout whose keys hit the cache links artifacts instead of compiling. Lake's own artifact cache (`LAKE_ARTIFACT_CACHE`) is the bar. Steps: (1) the local cache, store and restore by key (`cache.rs`); (2) a size bound with eviction (least recently used, configurable) and the milestone suite for agents-16. | clone, agents-4, agents-16 | standard suite `success`; milestone suite for agents-16. | step 1 done (PR #3); step 2 open |
| P3 | **Build server.** A persistent daemon shared by every checkout of a project: graph and state in memory, file watching, the harness's server protocol (`flume server protocol` prints `flume-server 1`; `server start`/`stop`), memory within the caps. The harness then measures warm and cold. | noop, leaf, agents-4 (warm) | warm scenarios faster than cold ones and than `main`; cold scenarios not slower. | open |
| P4 | **Environment reuse.** Keep imported environments alive across compilations (e.g. fork from a process that has already imported a module's imports) where the artifacts stay byte-identical. Measure import cost first; Blake measured ~35% of a deep module's compile. | leaf, hub-interface, cold | standard suite `success`. | open |
| P5 | **Scheduling for cold builds.** Priorities from measured compile times of earlier builds, memory-aware concurrency, critical-path first. | cold, hub-interface | milestone suite `success` on cold. | open |
| P6 | **Scale.** 100,000-module projects: binary state, parallel status checks, incremental graph updates; no-op and leaf latency that does not grow with the project. | noop, leaf (release suite) | release suite shows noop and leaf within 2x of the medium corpora's. | open |
| P7 | **Finer invalidation than Lake.** Skip dependents whose outputs provably cannot change (for example when a public addition is unused), with the identity check able to catch a mistake. Research: may need Lean's help (see DESIGN.md on patches). | hub-interface, swarm | standard suite `success` with identical artifacts on every edit kind. | open |

## Later

- Builds across several machines with a shared remote cache.
- Moving to a newer Lean release (a `toolchain:` PR re-running every check).
