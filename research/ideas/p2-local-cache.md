# p2-local-cache: checkouts share compiled modules through a content-addressed cache

- **Merged:** (filled in by the maintainer on merge)
- **Milestone:** P2 (step 1)
- **Log entry:** ../log/2026-11-02-p2-local-cache.md (built, measured `failure` on noise twice) and ../log/2026-11-02-p2-local-cache-2.md (re-measured, `success`)
- **Scenarios improved:** clone (233-321x faster than `main`, 1.5x faster than Lake)

## Problem

A fresh checkout of a project that another checkout on the same machine had already
built cost flume a full cold build (~40 s for 1,000 modules on 16 cores), while Lake
restores it from its artifact cache in ~0.3 s. In the baseline that one scenario put the
many-checkouts workflow at 0.09x of Lake and the overall number at 0.54x. The `break`
records showed the same gap in miniature: after a reverted breaking edit or a deleted
artifact, Lake restored the module from its cache (0 jobs) and flume recompiled it.

## Mechanism

After every successful compile, flume makes the outputs read-only and hard-links them into
a cache directory named by their blake3 content hash (`art/`), then records the module's
key, the outputs' hashes and sizes, and the compile's messages (`key/<os>-<arch>/`).
Before compiling a module that is not up to date, flume looks its key up; on a hit it
removes the module's old outputs (as before a compile), hard-links every cached artifact
into the build directory (copying when the file system refuses a link), checks each
size, and replays warnings. A miss, a missing artifact, or a wrong size falls back to the
compile, which stores the outputs again and so repairs the cache.

The artifacts stay byte-identical because a cache entry is used only for the exact key a
compile would have been checked against: the key hashes everything Lake traces
(toolchain, package, options, module name, source, and the imports' artifact traces) and
no paths, so equal keys are interchangeable in Lake's own sense (Lake's cache maps the
same input hash to outputs). The harness checks identity after the clone build, after
every scenario, and after the repair step, which now restores from the cache.

## Where it lives

- `crates/flume/src/cache.rs`: location (`FLUME_CACHE_DIR`, `XDG_CACHE_HOME`, `HOME`;
  `FLUME_CACHE=off`), `Cache::store`, `Cache::lookup`, `Cache::restore`.
- `crates/flume/src/build.rs`: `Shared::restore` (lookup, clear, link, stat) and the store
  at the end of `Shared::compile`; `Status::Restored`.
- `tests/check_cache.py`: end-to-end check (second checkout, warnings, deleted artifacts,
  edits, damaged cache, cache off).

## Measured gains

`research/results/20261102T225020Z-2e698309-15f5b59.json` (standard suite, `--targets
clone,agents-4 --repeat 5 --repeat-fast 20`, 16 CPUs):

```
speedup of candidate vs main: cold=1.002x, noop=0.983x, leaf=0.990x, newfile=1.023x, hub-private=1.000x, hub-interface=0.995x, swarm=1.005x, clone=268.884x, agents-4=1.016x, workflow:single=0.999x, workflow:many=16.528x, workflow:cold=1.002x, overall=2.549x
speedup of candidate vs lake: cold=1.036x, noop=3.696x, leaf=1.392x, newfile=2.026x, hub-private=1.433x, hub-interface=1.042x, swarm=1.205x, clone=1.517x, agents-4=1.422x, workflow:single=1.630x, workflow:many=1.469x, workflow:cold=1.036x, overall=1.354x
verdict: success [targets: clone, agents-4]
```

`clone` medians: 44.61 s to 0.19 s (deep), 48.33 s to 0.18 s (layered), 45.92 s to 0.14 s
(wide); Lake takes 0.25-0.27 s. The repair step after deleted artifacts went from 2 jobs to
0, as in Lake. Two earlier standard runs of the same code said `failure`, each on a
different scenario that never regressed again (see the log entries).

## Caveats

- **No size bound yet.** Every compile's outputs stay in the cache until it is deleted;
  hard links make the cache's own size small while checkouts still hold the files, but
  outputs replaced in every checkout are kept. P2 step 2 adds a bound with eviction.
- **Hard links share inodes.** Linking a cached file into another checkout changes the
  inode's ctime, so the next build of every checkout sharing it re-hashes that output once
  (the status check sees a change, the content check accepts it). Cheap, but it grows with
  the number of checkouts sharing files; an output status that ignores ctime for
  read-only cached files would avoid it.
- **The cache is trusted like Lake's.** Restored hashes come from the entry; a cached
  file changed in place with its size kept would go unnoticed until the harness or a
  rebuild caught it. Files are read-only to discourage that, as in Lake.
