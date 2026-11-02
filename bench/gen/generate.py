#!/usr/bin/env python3
"""Generate a synthetic, self-contained Lean 4 package for benchmarking build tools.

The generated package imports only Lean core (`Init`/`Std`), so it builds with
nothing but the toolchain in `lean-toolchain`. Its module import graph is a DAG
whose shape is controlled by `--shape`, and every module contains a mix of
definitions and theorems (`omega`, `decide`, `simp`, `deriving` handlers) whose
elaboration cost is controlled by `--work`. Generation is deterministic in the
config, so everyone who runs the same config gets a byte-identical corpus.

By default (`module_system: true`) every file is a Lean module (`module`,
`public import`, a `public section`, `@[expose]` on the chained `val`, a few
`private` declarations), so build tools can tell interface changes from
private ones. `module_system: false` writes ordinary (non-module) files with
the same declarations, as a guard for projects that do not use modules.

Usage:
  python3 bench/gen/generate.py --config bench/configs/medium-layered.json --out WORKDIR
  python3 bench/gen/generate.py --shape wide --modules 300 --seed 7 --out WORKDIR

Output layout (WORKDIR):
  lakefile.toml, lean-toolchain, Corpus/M00001.lean ..., corpus.json

`corpus.json` records the config, every module's imports, graph statistics,
and the scenario targets the benchmark harness edits (`targets`: the leaf, the
hub, the swarm, and one module per simulated agent). The statistics are
computed in time linear in the graph plus a bounded number of reachability
searches, so 100,000-module corpora generate in about a minute.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import random
import shutil
import sys
from pathlib import Path

GENERATOR_VERSION = 5

SHAPES = ("layered", "deep", "wide")

DEFAULTS = {
    "shape": "layered",
    "modules": 300,
    "seed": 1,
    # mean number of imports per module (shape-dependent interpretation)
    "imports_mean": 3.0,
    # number of generated declarations per module (beyond the fixed `val` chain)
    "decls_per_module": 6,
    # 1 (light) .. 3 (heavy): scales the size of `decide` computations
    "work": 2,
    # shape-specific knobs
    "chains": 8,        # deep: number of parallel chains
    "cross_prob": 0.3,  # deep: probability of an extra cross-chain import
    "hubs": 16,         # wide: number of hub modules everyone imports from
    # Lean module system (`module`, `public import`, public/private/expose)
    "module_system": True,
}

MAX_MODULES = 100_000   # module names are Corpus.M00000 .. Corpus.M99999
HUB_CANDIDATES = 48     # modules whose exact transitive-dependent count is computed
SWARM_SIZE = 24
AGENT_SLOTS = 16


def module_name(i: int) -> str:
    return f"Corpus.M{i:05d}"


def module_path(i: int) -> str:
    return f"Corpus/M{i:05d}.lean"


# ---------------------------------------------------------------------------
# DAG shapes
# ---------------------------------------------------------------------------

def poisson_like(rng: random.Random, mean: float, lo: int, hi: int) -> int:
    """Cheap integer draw around `mean`, clamped to [lo, hi]."""
    if mean <= 0:
        return lo
    k = 0
    p = math.exp(-mean)
    s = p
    u = rng.random()
    while u > s and k < hi:
        k += 1
        p *= mean / k
        s += p
    return max(lo, min(hi, k))


def shape_layered(cfg: dict, rng: random.Random) -> tuple[list[list[int]], list[int]]:
    """Random layered DAG: module i sits in a layer and imports modules from earlier
    layers, preferring recent ones. Bushy, moderate depth (~sqrt(N) layers)."""
    n = cfg["modules"]
    layers = max(2, int(math.sqrt(n)))
    layer_of = [min(layers - 1, i * layers // n) for i in range(n)]
    by_layer: list[list[int]] = [[] for _ in range(layers)]
    for i, l in enumerate(layer_of):
        by_layer[l].append(i)
    imports: list[list[int]] = [[] for _ in range(n)]
    for i in range(n):
        l = layer_of[i]
        if l == 0:
            continue
        k = poisson_like(rng, cfg["imports_mean"], 1, 12)
        chosen: set[int] = set()
        for _ in range(k):
            # distance-biased layer choice: geometric with p = 0.5
            d = 1
            while d < l and rng.random() < 0.5:
                d += 1
            src_layer = max(0, l - d)
            chosen.add(rng.choice(by_layer[src_layer]))
        imports[i] = sorted(chosen)
    return imports, layer_of


def shape_deep(cfg: dict, rng: random.Random) -> tuple[list[list[int]], list[int]]:
    """A few long parallel chains with occasional cross links: very long critical
    path (N / chains), little width. Stresses scheduling of the critical path."""
    n = cfg["modules"]
    chains = max(1, min(cfg["chains"], n))
    imports: list[list[int]] = [[] for _ in range(n)]
    depth = [0] * n
    for i in range(n):
        if i >= chains:
            prev = i - chains
            imports[i].append(prev)
            depth[i] = depth[prev] + 1
            if rng.random() < cfg["cross_prob"] and i > chains:
                other = rng.randrange(0, i)
                if other != prev:
                    imports[i].append(other)
                    depth[i] = max(depth[i], depth[other] + 1)
        imports[i] = sorted(set(imports[i]))
    return imports, depth


def shape_wide(cfg: dict, rng: random.Random) -> tuple[list[list[int]], list[int]]:
    """A small layered core of hub modules that almost everything imports, then a
    wide, shallow fan of modules each importing a couple of hubs and rarely one
    other non-hub module. Editing a hub forces a near-total rebuild."""
    n = cfg["modules"]
    hubs = max(1, min(cfg["hubs"], n // 2))
    imports: list[list[int]] = [[] for _ in range(n)]
    layer = [0] * n
    for i in range(hubs):
        if i > 0:
            k = poisson_like(rng, 1.5, 1, 4)
            imports[i] = sorted({rng.randrange(0, i) for _ in range(k)})
            layer[i] = 1 + max(layer[j] for j in imports[i])
    for i in range(hubs, n):
        k = poisson_like(rng, cfg["imports_mean"], 1, 6)
        chosen = {rng.randrange(0, hubs) for _ in range(k)}
        if i > hubs and rng.random() < 0.15:
            chosen.add(rng.randrange(hubs, i))
        imports[i] = sorted(chosen)
        layer[i] = 1 + max(layer[j] for j in imports[i])
    return imports, layer


SHAPE_FNS = {"layered": shape_layered, "deep": shape_deep, "wide": shape_wide}


# ---------------------------------------------------------------------------
# Module contents
# ---------------------------------------------------------------------------

def decide_decl(idx: int, rng: random.Random, work: int) -> str:
    """A closed numeric fact proved by `decide`; cost grows with `work`."""
    base = {1: 12, 2: 24, 3: 40}[work]
    m = base + rng.randrange(0, base // 2)
    kind = rng.randrange(0, 4)
    if kind == 3:
        # Primality sieve by `decide`: the kernel evaluates the nested `all`, so
        # this is the main knob for making a module expensive to elaborate.
        pm = {1: 30, 2: 60, 3: 120}[work] + rng.randrange(0, 20)
        primes = [k for k in range(pm) if k > 1 and all(k % d for d in range(2, k))]
        return (f"theorem primes_below_{idx} :\n"
                f"    (List.range {pm}).filter (fun n => decide (n > 1) && (List.range n).all (fun d => d < 2 || n % d != 0))\n"
                f"      = {primes} := by\n"
                f"  decide\n")
    if kind == 0:
        total = m * (m - 1) // 2
        return (f"theorem sum_range_{idx} : (List.range {m}).foldl (· + ·) 0 = {total} := by\n"
                f"  decide\n")
    if kind == 1:
        sq = sum(k * k for k in range(m))
        return (f"theorem sum_squares_{idx} : ((List.range {m}).map (fun k => k * k)).foldl (· + ·) 0 = {sq} := by\n"
                f"  decide\n")
    evens = [k for k in range(m) if k % 2 == 0]
    return (f"theorem evens_{idx} : (List.range {m}).filter (fun k => k % 2 == 0) = {evens} := by\n"
            f"  decide\n")


def omega_decl(idx: int, rng: random.Random) -> str:
    a, b = rng.randrange(1, 6), rng.randrange(1, 6)
    return (f"theorem lin_{idx} (x y z : Nat) (h₁ : x + {a} * y ≤ z) (h₂ : z ≤ x + {a} * y + {b}) :\n"
            f"    z - x - {a} * y ≤ {b} ∧ x ≤ z := by\n"
            f"  omega\n")


def simp_decl(idx: int, rng: random.Random) -> str:
    kind = rng.randrange(0, 3)
    if kind == 0:
        return (f"theorem len_app_{idx} (xs ys : List Nat) : (xs ++ ys ++ []).length = ys.length + xs.length := by\n"
                f"  simp [Nat.add_comm]\n")
    if kind == 1:
        return (f"theorem rev_rev_{idx} (xs : List Nat) : xs.reverse.reverse.length = xs.length := by\n"
                f"  simp\n")
    return (f"theorem map_id_{idx} (xs : List Nat) : (xs.map fun x => x + 0) = xs := by\n"
            f"  simp\n")


def inductive_decl(idx: int, rng: random.Random, expose: str = "") -> str:
    k = rng.randrange(2, 5)
    ctors = "\n".join(f"  | c{j}" + (f" (n : Nat)" if j == k - 1 else "") for j in range(k))
    arms = "\n".join(
        f"  | .c{j}" + (f" n => n + {j}" if j == k - 1 else f" => {j}") for j in range(k))
    return (f"inductive Shape{idx} where\n{ctors}\n  deriving DecidableEq, Repr, Inhabited, Hashable\n\n"
            f"{expose}def Shape{idx}.size : Shape{idx} → Nat\n{arms}\n\n"
            f"theorem Shape{idx}.size_last (n : Nat) : (Shape{idx}.c{k - 1} n).size = n + {k - 1} := rfl\n")


def structure_decl(idx: int, rng: random.Random) -> str:
    return (f"structure Point{idx} where\n  x : Nat\n  y : Nat\n  deriving DecidableEq, Repr\n\n"
            f"def Point{idx}.norm1 (p : Point{idx}) : Nat := p.x + p.y\n\n"
            f"theorem Point{idx}.norm1_ge_x (p : Point{idx}) : p.x ≤ p.norm1 := by\n"
            f"  unfold Point{idx}.norm1\n  omega\n")


def val_body(i: int, imports: list[int], consts: list[int]) -> str:
    """The right-hand side of module i's `val`, as Lean source over `n`."""
    if not imports:
        return f"n + {consts[i]}"
    terms = " + ".join(f"{module_name(j)}.val (n + {k})" for k, j in enumerate(imports))
    return f"{terms} + {consts[i]}"


def render_module(i: int, imports: list[int], all_imports: list[list[int]], consts: list[int], meta: str,
                  cfg: dict, rng: random.Random) -> str:
    name = module_name(i)
    mod = cfg["module_system"]
    lines = []
    if mod:
        lines.append("module")
        lines.append("")
    for j in imports:
        lines.append(f"{'public import' if mod else 'import'} {module_name(j)}")
    if imports:
        lines.append("")
    lines.append(f"/-! Module `{name}` ({meta}).\n"
                 f"Generated by `bench/gen/generate.py`; do not edit by hand. -/")
    lines.append("")
    if mod:
        lines.append("public section")
        lines.append("")
    lines.append(f"namespace {name}")
    lines.append("")
    expose = "@[expose] " if mod else ""
    # The `val` chain gives every module a real dependency on the declarations of
    # its imports, so a change upstream is semantically visible downstream. `val`
    # is exposed (its body is part of the module's interface) because dependents
    # restate it.
    if imports:
        haves = "\n".join(f"  have h{k} := {module_name(j)}.val_ge (n + {k})" for k, j in enumerate(imports))
        lines.append("/-- Value chained through this module's imports. -/")
        lines.append(f"{expose}def val (n : Nat) : Nat :=\n  {val_body(i, imports, consts)}")
        lines.append("")
        lines.append(f"theorem val_ge (n : Nat) : n ≤ val n := by\n  unfold val\n{haves}\n  omega")
        lines.append("")
        # Restate each import's definition body by `rfl`, so this module's build
        # depends on the *bodies* of its imports, not only on their statements:
        # a build tool that skipped rebuilding dependents after a body change
        # would leave these proofs stale.
        for k, j in enumerate(imports):
            lines.append(f"theorem imp_unfold_{k} (n : Nat) : {module_name(j)}.val n = {val_body(j, all_imports[j], consts)} := rfl")
        lines.append("")
        # The same fact by unfolding with `simp`: its proof term records how the
        # import's body was rewritten, so this module's artifacts change (without
        # any error) when that body changes in a definitionally equal way. A tool
        # that skipped such a rebuild would leave stale artifacts behind.
        j0 = imports[0]
        # (the two arithmetic lemmas are used only after such a change)
        lines.append(f"set_option linter.unusedSimpArgs false in\n"
                     f"theorem imp_body (n : Nat) : {module_name(j0)}.val n = {val_body(j0, all_imports[j0], consts)} := by\n"
                     f"  simp only [{module_name(j0)}.val, Nat.zero_mul, Nat.add_zero]")
    else:
        lines.append("/-- Root value of the chain. -/")
        lines.append(f"{expose}def val (n : Nat) : Nat := {val_body(i, imports, consts)}")
        lines.append("")
        lines.append("theorem val_ge (n : Nat) : n ≤ val n := by\n  unfold val\n  omega")
    lines.append("")
    # Private helpers: not part of the interface, so changing them need not
    # rebuild dependents (in module-system corpora).
    c = consts[i]
    lines.append(f"private def aux (n : Nat) : Nat := n * {c} + 1")
    lines.append("")
    lines.append(f"private theorem aux_pos (n : Nat) : 0 < aux n := by\n  unfold aux\n  omega")
    lines.append("")
    gens = [decide_decl, omega_decl, simp_decl, inductive_decl, structure_decl]
    weights = [3, 2, 2, 1, 1]
    for d in range(cfg["decls_per_module"]):
        gen = rng.choices(gens, weights=weights, k=1)[0]
        if gen is decide_decl:
            lines.append(gen(d, rng, cfg["work"]))
        elif gen is inductive_decl:
            lines.append(gen(d, rng, expose))   # its `rfl` lemma unfolds `size`
        else:
            lines.append(gen(d, rng))
    lines.append(f"end {name}")
    if mod:
        lines.append("")
        lines.append("end")
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------

def load_config(args: argparse.Namespace) -> dict:
    cfg = dict(DEFAULTS)
    if args.config:
        with open(args.config, encoding="utf-8") as f:
            cfg.update(json.load(f))
        cfg.setdefault("name", Path(args.config).stem)
    for key in DEFAULTS:
        val = getattr(args, key, None)
        if val is not None:
            cfg[key] = val
    if args.name:
        cfg["name"] = args.name
    cfg.setdefault("name", f"{cfg['shape']}-{cfg['modules']}-s{cfg['seed']}")
    if cfg["shape"] not in SHAPES:
        sys.exit(f"unknown shape {cfg['shape']!r}; choose from {SHAPES}")
    if cfg["work"] not in (1, 2, 3):
        sys.exit("--work must be 1, 2, or 3")
    if not 2 <= cfg["modules"] <= MAX_MODULES:
        sys.exit(f"--modules must be between 2 and {MAX_MODULES}")
    if isinstance(cfg["module_system"], str):
        cfg["module_system"] = cfg["module_system"].lower() in ("1", "true", "yes")
    cfg["generator_version"] = GENERATOR_VERSION
    return cfg


def reach_count(start: int, dependents: list[list[int]], seen: bytearray, stamp: int) -> int:
    """Number of modules that transitively import `start` (what an edit to it can force to rebuild).

    `seen` is a reusable bytearray; `stamp` (1..255) marks this search's visits.
    """
    count = 0
    stack = list(dependents[start])
    while stack:
        k = stack.pop()
        if seen[k] == stamp:
            continue
        seen[k] = stamp
        count += 1
        stack.extend(dependents[k])
    return count


def spread(pool: list[int], k: int) -> list[int]:
    """k elements of `pool` (sorted), evenly spaced."""
    if k <= 0 or not pool:
        return []
    if len(pool) <= k:
        return list(pool)
    step = len(pool) / k
    return [pool[int(step * t)] for t in range(k)]


def graph_stats(imports: list[list[int]], layer: list[int]) -> tuple[dict, dict, list[int], list[int | None]]:
    """Graph statistics, scenario targets, per-module height, and transitive-dependent
    counts (exact for the modules where they matter, None elsewhere)."""
    n = len(imports)
    dependents: list[list[int]] = [[] for _ in range(n)]
    for i, deps in enumerate(imports):
        for j in deps:
            dependents[j].append(i)
    # Every shape only imports lower indices, so index order is topological.
    depth = [0] * n
    for i in range(n):
        if imports[i]:
            depth[i] = 1 + max(depth[j] for j in imports[i])
    height = [0] * n   # longest path to a module nobody imports
    for i in range(n - 1, -1, -1):
        if dependents[i]:
            height[i] = 1 + max(height[k] for k in dependents[i])
    trans: list[int | None] = [None] * n
    seen = bytearray(n)
    searches = 0

    def count(i: int) -> int:
        nonlocal searches
        if trans[i] is None:
            searches += 1
            if searches % 255 == 0:
                seen[:] = bytes(n)
            trans[i] = reach_count(i, dependents, seen, searches % 255 or 255)
        return trans[i]

    leaves = [i for i in range(n) if not dependents[i]]
    for i in leaves:
        trans[i] = 0
    # hub: the most widely (transitively) imported module among the likeliest
    # candidates: the tallest modules and those with the most direct dependents
    by_height = sorted(range(n), key=lambda i: (-height[i], i))[:HUB_CANDIDATES // 2]
    by_fanout = sorted(range(n), key=lambda i: (-len(dependents[i]), i))[:HUB_CANDIDATES // 2]
    hub = max(set(by_height) | set(by_fanout), key=lambda i: (count(i), -i))
    # leaf: the deepest module nobody imports (longest import chain to rebuild through)
    leaf = max(leaves, key=lambda i: (layer[i], i))
    # swarm and agents: modules with few dependents, spread over the DAG, as agents
    # working in parallel mostly touch recent, little-imported modules
    low = [i for i in range(n) if height[i] <= 1 and i != leaf and i != hub]
    if len(low) < SWARM_SIZE + AGENT_SLOTS:
        low = sorted(set(low) | set(sorted(range(n), key=lambda i: (height[i], i))[: SWARM_SIZE + AGENT_SLOTS + 2])
                     - {leaf, hub})
    k_swarm = max(1, min(SWARM_SIZE, n // 10))
    swarm = spread(low, k_swarm)
    rest = [i for i in low if i not in set(swarm)]
    agents = spread(rest, min(AGENT_SLOTS, len(rest)))
    for i in swarm + agents:
        count(i)
    stats = {
        "modules": n,
        "edges": sum(len(d) for d in imports),
        "critical_path": (max(depth) + 1) if n else 0,
        "leaves": len(leaves),
        "roots": sum(1 for d in imports if not d),
        "max_transitive_dependents": trans[hub],
    }
    targets = {
        "leaf": module_name(leaf),
        "hub": module_name(hub),
        "hub_transitive_dependents": trans[hub],
        "hub_direct_dependents": len(dependents[hub]),
        "swarm": [module_name(i) for i in swarm],
        "agents": [module_name(i) for i in agents],
    }
    return stats, targets, height, trans


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--config", help="JSON config file (keys as in DEFAULTS)")
    ap.add_argument("--out", required=True, help="output directory (created; must be empty or absent)")
    ap.add_argument("--name", help="corpus name recorded in corpus.json")
    ap.add_argument("--toolchain", help="lean-toolchain file to copy (default: the repository's)")
    ap.add_argument("--force", action="store_true", help="overwrite a non-empty output directory")
    for key, default in DEFAULTS.items():
        kind = str if isinstance(default, bool) else type(default)
        ap.add_argument(f"--{key.replace('_', '-')}", dest=key, type=kind, default=None,
                        help=f"(default {default})")
    args = ap.parse_args()
    cfg = load_config(args)

    out = Path(args.out)
    if out.exists() and any(out.iterdir()):
        if not args.force:
            sys.exit(f"{out} is not empty (use --force to overwrite)")
        shutil.rmtree(out)
    (out / "Corpus").mkdir(parents=True, exist_ok=True)

    rng = random.Random(f"blake-corpus-v{GENERATOR_VERSION}-{cfg['seed']}-{cfg['shape']}-{cfg['modules']}")
    imports, layer = SHAPE_FNS[cfg["shape"]](cfg, rng)
    n = cfg["modules"]

    total_lines = 0
    sha = hashlib.sha256()
    modules = []
    consts = [random.Random(f"{cfg['seed']}-{i}-const").randrange(1, 9) for i in range(n)]
    for i in range(n):
        mrng = random.Random(f"{cfg['seed']}-{i}")
        meta = f"layer {layer[i]}, {len(imports[i])} imports"
        text = render_module(i, imports[i], imports, consts, meta, cfg, mrng)
        path = out / module_path(i)
        data = text.encode("utf-8")
        path.write_bytes(data)
        total_lines += text.count("\n")
        sha.update(data)
        modules.append({"name": module_name(i), "file": module_path(i),
                        "imports": [module_name(j) for j in imports[i]], "layer": layer[i]})

    (out / "lakefile.toml").write_text(encoding="utf-8", data=
        "# Generated benchmark corpus (bench/gen/generate.py); do not edit.\n"
        f"name = \"corpus\"\n"
        "defaultTargets = [\"Corpus\"]\n\n"
        "[[lean_lib]]\n"
        "name = \"Corpus\"\n"
        "globs = [\"Corpus.+\"]\n")

    # the stock toolchain pinned in the repository's `lean-toolchain`
    toolchain_src = Path(args.toolchain) if args.toolchain else Path(__file__).resolve().parents[2] / "lean-toolchain"
    toolchain = toolchain_src.read_text(encoding="utf-8")
    (out / "lean-toolchain").write_text(toolchain, encoding="utf-8")

    stats, targets, height, trans = graph_stats(imports, layer)
    stats["lines"] = total_lines
    stats["content_sha256"] = sha.hexdigest()
    (out / "corpus.json").write_text(encoding="utf-8", data=json.dumps({
        "config": cfg,
        "stats": stats,
        "targets": targets,
        "modules": [dict(m, height=height[i], transitive_dependents=trans[i]) for i, m in enumerate(modules)],
    }, indent=1) + "\n")

    print(f"generated {cfg['name']}: {n} modules, {total_lines} lines, {stats['edges']} edges, "
          f"critical path {stats['critical_path']}, leaves {stats['leaves']}, "
          f"hub {targets['hub']} ({targets['hub_transitive_dependents']} transitive dependents), "
          f"{'module system' if cfg['module_system'] else 'no module system'} -> {out}")


if __name__ == "__main__":
    main()
