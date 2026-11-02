# p2-local-cache: a shared, content-addressed artifact cache on one machine (P2, step 1)

- **Date:** 2026-11-02
- **Author:** Claude (Opus 5.5), owner loop, machine `2e698309` (i9-14900K, 16 of 32 CPUs, 32 GB cap)
- **Milestone:** P2 (step 1: local cache, store and restore by key)
- **Outcome:** (in progress)
- **Branch / PR:** research/p2-local-cache
- **Result files:** (pending)

## Plan

The baseline (`2026-11-02-baseline.md`) puts flume at 0.007x of Lake on `clone`: Lake
restores a fresh checkout from its artifact cache in ~0.3 s with 0 jobs, flume recompiles
all 1,010 modules in ~40 s. That one scenario drags the many-checkouts workflow to 0.09x
and the overall number to 0.54x. The baseline's `break` records show the same gap in a
smaller form: after a breaking edit is reverted, Lake restores the hub from its cache
(0.19 s, and 0 jobs in the repair step), flume recompiles it (0.7 s, 2 jobs).

Step 1 of P2 is a cache shared by every checkout on the machine, modeled on Lake's own
(`Cache.saveArtifact`, `restoreArtifact` in `Lake/Build/Common.lean`):

- **Where:** `$FLUME_CACHE_DIR`, else `$XDG_CACHE_HOME/flume`, else `~/.cache/flume`
  (the XDG convention every tool follows; `FLUME_CACHE_DIR=off` disables the cache).
- **What:** `art/<blake3 of content>`: each artifact once, by content; `key/<key>.json`:
  a module's outputs (facet, content hash, size) and its messages, by the module's key.
  The key already hashes everything Lake traces (toolchain, package, options, name,
  source, the imports' artifact traces), so equal keys mean Lake would treat the outputs
  as interchangeable; Lake's own cache uses the same input-hash-to-outputs mapping.
- **Store:** after a successful compile, each output is made read-only (as Lake does) and
  hard-linked into `art/` (copied through a temporary file and renamed if linking fails),
  then the key entry is written atomically.
- **Restore:** a module that is not up to date but whose key is in the cache gets its old
  outputs removed and every artifact hard-linked (or copied) into `.lake/build`, exactly
  where a compile would have put it, so the build directory keeps Lake's layout. It prints
  nothing (as Lake prints nothing for a replay), unless the stored messages contain
  warnings, which are replayed.

Targets: `clone` and `agents-4` (the standard suite cannot run `agents-16`; size bounds
and eviction are the next step of P2). I expect `clone` to drop from ~40 s to well under
a second and every other scenario to stay put: the store adds a link, a chmod, and one
small file per compiled module.

How I will know: the quick suite stays correct (identical artifacts after every build,
including the clone and the repair step), unit tests cover store/restore/fallbacks, and
the standard suite with `--targets clone,agents-4` says `success`.

## What was done

## Measurements

## Conclusion
