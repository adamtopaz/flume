# flume

flume is a build system for [Lean 4](https://lean-lang.org) built to be fast first and
small enough to change quickly. It is a Rust driver plus a small Lean worker that use the
stock Lean toolchain as a library: flume decides what to build and runs the builds, and
Lean does everything that needs Lean's semantics.

- **Identical results.** For the same inputs flume writes `.olean`, `.ilean`, `.c`, and
  module-system artifacts byte-identical to stock Lake's, rebuilds a module exactly when
  Lake would, and fails exactly where Lake fails. The benchmark checks this after every
  build.
- **Fast.** On the benchmark's small corpora, version 0 already does no-op builds about
  5x faster than Lake and leaf edits 1.5x faster; the roadmap targets builds across many
  agents' checkouts and cold builds next.
- **A crowd-sourced research loop.** Anyone can clone this repository, point a coding
  agent at [`instructions.md`](instructions.md), and run one iteration on their own
  machine. The design, the roadmap, the benchmark, and the record of every attempt are
  in [`research/`](research/) and [`bench/`](bench/).

## Status

Version 0 builds `lakefile.toml` packages made of Lean libraries without dependencies.
Dependencies, `lakefile.lean`, editor support, and executables are roadmap milestones
([`research/ROADMAP.md`](research/ROADMAP.md)); flume reports an error for anything it
does not support yet rather than ignoring it.

## Use

```sh
nix develop                       # or have cargo (Rust 1.85+) and elan on PATH
cargo build --release
cd path/to/a/lean/package         # with a lakefile.toml
/path/to/flume/target/release/flume build
```

`flume build [TARGET...]` builds library or module targets (default: the package's
default targets); `-j N` sets the number of concurrent compilations (default:
`LEAN_NUM_THREADS`, else the CPUs available); `flume query MOD:FACET` prints an artifact's
path; `flume clean` removes the build directory and flume's state.

## License

Apache License 2.0, like Lean.
