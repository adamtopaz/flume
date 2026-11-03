# p1-zygote: compile modules in forked children of one pre-initialized Lean

- **Merged:** (filled in by the maintainer on merge)
- **Milestone:** P1
- **Log entry:** ../log/2026-11-02-p1-zygote.md
- **Scenarios improved:** hub-interface (all corpora, 6.5-9.5%), swarm (deep, layered), cold (wide; 5.5% on average)

## Problem

Every compile was a fresh `lean` process: `exec`, dynamic linking of `libleanshared`, and
Lean's initialization (every builtin parser, attribute, and extension) before reading the
file. `lean --version` takes 21-24 ms on an idle machine; under a 16-way build it costs
more. That is paid by each of the ~1,000 compiles of `cold` and `hub-interface` and on
every step of a chain in `swarm`. The same idea gave Blake 5-7% on its heavy scenarios
(github.com/adamtopaz/blake, `research/ideas/one-zygote.md`).

## Mechanism

`zygote.c` is a single-threaded C program linked against the toolchain's
`libleanshared`. It calls `lean_initialize()` once (what `lean`'s `lean::initializer`
does), prints `ready`, and serves requests on stdin: each request (id, stdout file, stderr
file, `LEAN_PATH`, `argv`) is forked at once into a child that runs the rest of
`lean_main` from v4.35.0's `src/util/shell.cpp`: `lean_init_search_path`,
`lean_enable_initializer_execution`, the stack size from `LEAN_STACK_SIZE_KB`, `getopt`
over `lean`'s option table into `lean_shell_options_process`,
`lean_io_mark_end_initialization`, the task manager, and `lean_shell_main` on a thread
with `lean`'s stack size (`lean_run_main`), with IO errors printed as `lean` prints them.
The zygote replies `<id> <exit code>` when the child exits. Each child gets
`PR_SET_PDEATHSIG`, so none outlives the zygote.

flume (`zygote.rs`) embeds the C source and compiles it with `cc -O2` on first use into
the artifact cache (`<cache>/zygote/<hash of source, toolchain prefix, version>/`). At a
build's first compile it starts the zygote on a background thread; compiles go to it once
it has printed `ready`, and run `lean` directly before that, so a build with one compile
waits for nothing. Each flume worker writes its request and waits for its own reply, so
the worker pool still bounds concurrency; at the end of the build all requests have been
answered, the zygote has no children, and it is killed and reaped.

The artifacts stay byte-identical because each child runs `lean`'s own driver code with
`lean`'s arguments, in a process initialized exactly as `lean` initializes itself; the
only difference is the search path's built-in entry, which `lean` derives from its own
location, so flume appends the toolchain's `lib/lean` to `LEAN_PATH` (where `lean` puts
it, last). Library arguments other than `-D` options (`--tstack`, `--profile`, `--run`)
run `lean` directly, since there the zygote could not match `lean`'s start-up exactly.

## Where it lives

- `crates/flume/src/zygote.c`: the fork server.
- `crates/flume/src/zygote.rs`: `Zygote::new` (enabled for Lean 4.35.0 with headers, not
  with `FLUME_ZYGOTE=off`), `run` (request and reply), `start` (compile, spawn, reader
  thread), `finish`, `compile` (the `cc` invocation).
- `crates/flume/src/build.rs`: `Shared::compile` asks the zygote first and falls back to
  `lean`; `build` calls `finish`.
- `tests/check_zygote.py`: builds with and without the zygote compared (artifacts,
  messages, failures, exit status).

## Measured gains

`research/results/20261103T004822Z-2e698309-b0d74fa.json` (standard suite, 5/20
repetitions, 16 CPUs):

```
speedup of candidate vs main: cold=1.055x, noop=1.016x, leaf=1.004x, newfile=1.008x, hub-private=0.995x, hub-interface=1.076x, swarm=1.037x, clone=1.028x, agents-4=1.012x, workflow:single=1.023x, workflow:many=1.020x, workflow:cold=1.055x, overall=1.032x
speedup of candidate vs lake: cold=1.103x, noop=4.418x, leaf=1.382x, newfile=1.891x, hub-private=1.396x, hub-interface=1.114x, swarm=1.217x, clone=1.711x, agents-4=1.427x, workflow:single=1.672x, workflow:many=1.562x, workflow:cold=1.103x, overall=1.423x
verdict: success [targets: leaf, newfile, hub-interface, swarm, cold]
```

`hub-interface` used 30-45 CPU-s less per ~900 compiles (~40-45 ms per compile) and peak
memory was a little lower.

## Caveats

- **Tied to Lean v4.35.0.** The option table and start-up sequence mirror that release's
  `lean_main`. A toolchain bump must re-check `src/util/shell.cpp` and update
  `LEAN_VERSION`; until then flume runs `lean` directly (correct, just slower).
- **Needs `cc`.** Without a C compiler on PATH (the Lean toolchain's bundled clang lacks
  POSIX headers) flume runs `lean`; a failed compile leaves a `failed` marker in the
  cache so that later builds do not retry.
- **Single-compile builds gain nothing**: the first compiles of every build run `lean`
  while the zygote initializes. A warm zygote in a build server (P3) would fix that.
- **`IO.appPath`** in a child is the zygote's path, not `lean`'s; a module that bakes it
  into a definition at elaboration time would differ. Blake made the same trade.
