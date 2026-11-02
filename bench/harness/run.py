#!/usr/bin/env python3
"""flume benchmark harness.

Measures stock Lake, flume `main`, and the candidate (the current checkout), and
at milestones also Blake (the research fork of Lake, at a pinned commit), on the
same machine, under the same CPU and memory caps, on generated corpora (see
`bench/gen/generate.py`). It checks that every tool produces artifacts
byte-identical to stock Lake's, decides whether the candidate improved on
`main`, and writes one JSON result file. Every tool is driven the same way:
`<exe> build` in the corpus directory.

Workflows and scenarios (`--suite` picks corpora and repetitions):

  cold workflow
    cold           first build of a fresh checkout, empty caches
  single-checkout workflow (edits in one working copy)
    noop           build with nothing changed: build-tool overhead
    leaf           edit the deepest module nobody imports (edit kind rotates)
    newfile        add a new module importing the leaf and the hub
    hub-private    edit the most widely imported module without changing its
                   interface (a proof, or a private theorem)
    hub-interface  change the hub's interface (a public theorem, a statement,
                   or an exposed body); dependents must be rebuilt or proven
                   unaffected
    swarm          edit up to 24 little-imported modules at once, mixed kinds
  many-checkouts workflow (agents sharing a machine)
    clone          a fresh checkout of the current sources, built while the
                   tool's shared cache already holds everything
    agents-4/16    4 or 16 built checkouts each edit their own module and
                   build at the same time (time until all are done)
  (untimed) break  two breaking edits to the hub, each must fail with exactly
                   as many failed jobs as stock Lake, then recover

Edit kinds and breaking edits are defined in `edits.py`. Every tool gets its
own copy of each corpus, and the tools are interleaved run by run (the same
edit is applied to every copy, then the tools build one after another in a
rotating order), so drift of the machine affects all tools alike.

Caches: every tool runs with Lake's artifact cache enabled
(`LAKE_ARTIFACT_CACHE=true`) in a private cache directory per tool and corpus,
shared by all of that tool's checkouts of the corpus, as a swarm of agents
would configure it. Remote cache downloads are disabled (`LAKE_NO_CACHE=1`).

Correctness: after *every* build, every artifact of every module (`.olean`,
`.ilean`, `.c`, and for module-system corpora also `.olean.server`,
`.olean.private`, `.ir`, `.ir.sig`) is hashed and compared with stock Lake's at
the same step. Artifacts are read from `.lake/build`; one that is not there
(served from the cache) is located with the tool's own `lake query`. A candidate
whose artifacts ever differ, whose builds fail, or which leaves processes behind
is INCORRECT regardless of speed.

Build servers: a tool may run a persistent server. It advertises one by
printing `flume-server 1` for `<exe> server protocol`; the harness then starts it
with `<exe> server start` (in its own systemd scope, under the same caps) and
measures the single-checkout and many-checkouts scenarios a second time with the
server warm (scenario names get the suffix `@warm`; every tool is measured again
so pairs stay interleaved), and stops it with `<exe> server stop`, after which no
process may remain. Builds with no server running are always measured too.

Tools: `lake` is the stock toolchain's own `lake` (the toolchain pinned in the
repository's `lean-toolchain`). `main` and `candidate` are flume built with
`cargo build --release` in a worktree of the merge-base with origin/main and in
this checkout (inside `nix develop` on NixOS if cargo is not on PATH). `blake`
is Blake (github.com/adamtopaz/blake) at the commit in `bench/blake.json`, built
with its own `lake build`. All of them run the same stock Lean.

Verdict (printed and stored): per corpus and scenario the candidate is compared
with `main` on *paired* runs (repetition i against repetition i; speedup =
median of main_i / candidate_i). Scenarios whose first run takes under 2 s get
--repeat-fast repetitions and a 7% threshold; longer ones get --repeat and 3%;
`cold` runs once and uses 7%; sequential (release-suite) runs compare medians
with a 10% threshold. Improved iff speedup >= 1 + t, regressed iff <= 1 - t.
Overall: `failure` if anything is incorrect or any scenario regressed; `success`
if a target scenario improved on some corpus (targets: `--targets`, default all
scenarios) and nothing regressed; otherwise `neutral`. `--rescore FILE`
recomputes a stored result's verdict under the current rule.

Suites:
  quick      small-*  (200 modules)   repeat 1/2, no agents-16: smoke test and CI
  standard   medium-* (1,000 modules) repeat 3/10, no agents-16: every iteration (~50 min on 16 cores)
  milestone  medium-* and large-layered (10,000 modules), every scenario, Blake too: milestones (hours)
  release    huge-*   (100,000 modules), tools one after another, cheap scenarios only (hours)

Only a run with its suite's defaults (configs, tools, scenarios, repetitions),
a clean git tree, and an enforced CPU cap is *standard*; standard runs are
written to research/results/, anything else to bench/work/results/.

Typical use:
  python3 bench/harness/run.py                       # the standard suite
  python3 bench/harness/run.py --suite quick         # smoke test
  python3 bench/harness/run.py --suite milestone --targets hub-interface,swarm

Caps: `--cpus` pins the harness and everything it spawns to those CPUs (Linux
only) and sets LEAN_NUM_THREADS to their count. `--mem-gb` wraps each build
(and each group of concurrent agent builds, and a server) in a transient systemd
user scope with MemoryMax where available; the scope also accounts for the
whole process tree and is used to find and kill stragglers. Defaults come from
`bench/local.json` (gitignored) or, failing that, half of the machine.

The environment is scrubbed of every LAKE_*/LEAN_*/ELAN_TOOLCHAIN/XDG_CACHE_HOME
variable; each tool gets private cache, config, and temp directories.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import platform
import re
import resource
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import edits  # noqa: E402

ROOT = HERE.parents[1]
BENCH = ROOT / "bench"
CONFIGS = BENCH / "configs"
WORK = BENCH / "work"
RESULTS = ROOT / "research" / "results"
GEN = BENCH / "gen" / "generate.py"
BLAKE_PIN = BENCH / "blake.json"
SCOPE_STATS = HERE / "scope_stats.sh"
MULTI_BUILD = HERE / "multi_build.py"
SCHEMA_VERSION = 3

COLD = ("cold",)
SINGLE = ("noop", "leaf", "newfile", "hub-private", "hub-interface", "swarm")
MANY = ("clone", "agents-4", "agents-16")
SCENARIOS = COLD + SINGLE + MANY
WORKFLOWS = {"single": SINGLE, "many": MANY, "cold": COLD}
AGENTS = {"agents-4": 4, "agents-16": 16}
SUITES = {
    "quick": {"configs": ["small-*"], "exclude": ["agents-16"], "repeat": 1, "repeat_fast": 2, "sequential": False,
              "tools": "lake,main,candidate"},
    "standard": {"configs": ["medium-*"], "exclude": ["agents-16"], "repeat": 3, "repeat_fast": 10, "sequential": False,
                 "tools": "lake,main,candidate"},
    "milestone": {"configs": ["medium-*", "large-layered"], "exclude": [], "repeat": 3, "repeat_fast": 10,
                  "sequential": False, "tools": "lake,blake,main,candidate"},
    "release": {"configs": ["huge-*"], "exclude": ["hub-interface", "swarm", "clone", "agents-4", "agents-16"],
                "repeat": 1, "repeat_fast": 5, "sequential": True, "tools": "lake,main,candidate"},
}
MODULE_FACETS = ("olean", "olean.server", "olean.private", "ilean", "ir", "ir.sig", "c")
LEGACY_FACETS = ("olean", "ilean", "c")
BUILD_TIMEOUT_S = 1800
NOISE_FLOOR = 0.03        # paired comparison of scenarios taking >= FAST_SECONDS
NOISE_FLOOR_FAST = 0.07   # paired comparison of scenarios under FAST_SECONDS
FAST_SECONDS = 2.0
COLD_THRESHOLD = 0.07     # cold runs once, so the comparison is a single pair
SEQUENTIAL_THRESHOLD = 0.10
NOISE_RULE = {"statistic": "median of paired per-repetition ratios main_i / candidate_i",
              "floor": NOISE_FLOOR, "floor_fast": NOISE_FLOOR_FAST, "fast_seconds": FAST_SECONDS,
              "cold_threshold": COLD_THRESHOLD, "sequential_threshold": SEQUENTIAL_THRESHOLD}
SCRUB_PREFIXES = ("LAKE_", "LEAN_")
SCRUB_EXACT = ("LEAN", "ELAN_TOOLCHAIN", "XDG_CACHE_HOME", "TMPDIR", "CC", "AR", "CURL")
SERVER_PROTOCOL = "flume-server 1"


def log(msg: str) -> None:
    print(f"[bench {dt.datetime.now().strftime('%H:%M:%S')}] {msg}", flush=True)


def banner(msg: str) -> None:
    print("\n" + "!" * 78 + f"\n! {msg}\n" + "!" * 78 + "\n", flush=True)


def run(cmd: list[str], cwd: Path | None = None, env: dict | None = None, check: bool = True,
        timeout: float | None = None) -> subprocess.CompletedProcess:
    proc = subprocess.run(cmd, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                          encoding="utf-8", errors="replace", timeout=timeout)
    if check and proc.returncode != 0:
        print(proc.stdout[-4000:], file=sys.stderr)
        raise SystemExit(f"command failed ({proc.returncode}): {' '.join(cmd)}")
    return proc


def git(*args: str, cwd: Path = ROOT, check: bool = True) -> str:
    return run(["git", *args], cwd=cwd, check=check).stdout.strip()


def tree_dirty() -> bool:
    """Uncommitted changes to tracked files, ignoring research/ (log entries are written during an iteration)."""
    return bool(git("status", "--porcelain", "--untracked-files=no", "--", ".", ":(exclude)research"))


def sha256_file(p: Path) -> str:
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def copy_tree(src: Path, dst: Path, with_build: bool) -> None:
    """Copy a checkout (reflinks where the filesystem supports them)."""
    if dst.exists():
        shutil.rmtree(dst)
    if with_build and shutil.which("cp"):
        subprocess.run(["cp", "-a", "--reflink=auto", str(src), str(dst)], check=True)
    else:
        shutil.copytree(src, dst, symlinks=True,
                        ignore=None if with_build else shutil.ignore_patterns(".lake", ".bench-build.log"))


# ---------------------------------------------------------------------------
# Caps and machine fingerprint
# ---------------------------------------------------------------------------

def parse_cpus(spec: str) -> list[int]:
    cpus: set[int] = set()
    for part in spec.split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            a, b = part.split("-", 1)
            cpus.update(range(int(a), int(b) + 1))
        else:
            cpus.add(int(part))
    return sorted(cpus)


def available_cpus() -> list[int]:
    if hasattr(os, "sched_getaffinity"):
        return sorted(os.sched_getaffinity(0))
    return list(range(os.cpu_count() or 1))


def total_mem_gb() -> int:
    try:
        with open("/proc/meminfo") as f:
            for line in f:
                if line.startswith("MemTotal:"):
                    return int(line.split()[1]) // (1024 * 1024)
    except OSError:
        pass
    try:
        return int(os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE") // (1024 ** 3))
    except (ValueError, OSError, AttributeError):
        return 0


def default_caps() -> dict:
    caps = {"cpus": None, "mem_gb": None}
    local = BENCH / "local.json"
    if local.exists():
        caps.update(json.loads(local.read_text(encoding="utf-8")))
    avail = available_cpus()
    if caps["cpus"] is None:
        half = avail[: max(1, len(avail) // 2)]
        caps["cpus"] = ",".join(str(c) for c in half)
    if caps["mem_gb"] is None:
        caps["mem_gb"] = max(1, total_mem_gb() // 2)
    return caps


def apply_cpu_cap(cpus: list[int]) -> tuple[list[int], bool]:
    """Pin to `cpus`; return the CPUs actually in effect and whether pinning worked."""
    if not hasattr(os, "sched_setaffinity"):
        return cpus, False
    try:
        os.sched_setaffinity(0, set(cpus))
    except OSError as e:
        log(f"warning: could not set CPU affinity: {e}")
        return cpus, False
    actual = sorted(os.sched_getaffinity(0))
    if actual != cpus:
        log(f"warning: requested cpus {cpus} but affinity is {actual} (outside this process's allowed set?)")
    return actual, True


def memory_controller_delegated() -> bool:
    uid = os.getuid()
    p = Path(f"/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/cgroup.controllers")
    try:
        return "memory" in p.read_text().split()
    except OSError:
        return False


def scope_support(mem_gb: int | None) -> tuple[bool, str]:
    """Whether transient systemd user scopes can be used, and how memory is enforced."""
    if platform.system() != "Linux" or not shutil.which("systemd-run"):
        return False, "unenforced"
    probe = subprocess.run(["systemd-run", "--user", "--scope", "-q", "-p", "MemoryMax=1G", "--", "true"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if probe.returncode != 0:
        return False, "unenforced"
    if not mem_gb:
        return True, "none"
    return True, ("systemd-run MemoryMax" if memory_controller_delegated() else "unenforced (memory controller not delegated)")


def cpu_model() -> str:
    try:
        with open("/proc/cpuinfo") as f:
            for line in f:
                if line.startswith("model name"):
                    return line.split(":", 1)[1].strip()
    except OSError:
        pass
    if platform.system() == "Darwin":
        out = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True)
        if out.returncode == 0:
            return out.stdout.strip()
    return platform.processor() or platform.machine()


def machine_id(cpu: str) -> str:
    seed = ""
    try:
        seed = Path("/etc/machine-id").read_text().strip()
    except OSError:
        if platform.system() == "Darwin":
            out = subprocess.run(["ioreg", "-rd1", "-c", "IOPlatformExpertDevice"], capture_output=True, text=True)
            m = re.search(r'"IOPlatformUUID" = "([^"]+)"', out.stdout)
            seed = m.group(1) if m else ""
    if not seed:
        seed = platform.node()
    return hashlib.sha256(f"{seed}|{cpu}".encode()).hexdigest()[:8]


# ---------------------------------------------------------------------------
# Environment hygiene, toolchains, and tools
# ---------------------------------------------------------------------------

def scrubbed_env() -> tuple[dict, list[str]]:
    env = {}
    scrubbed = []
    for k, v in os.environ.items():
        if k.startswith(SCRUB_PREFIXES) or k in SCRUB_EXACT:
            scrubbed.append(k)
            continue
        env[k] = v
    env.setdefault("LANG", "C.UTF-8")
    return env, sorted(scrubbed)


def toolchain_prefix(env: dict) -> Path:
    """The stock toolchain pinned in the repository's `lean-toolchain`."""
    return Path(run(["lean", "--print-prefix"], cwd=ROOT, env=env).stdout.strip())


def with_lean_cc(base: dict, stock_prefix: Path) -> dict:
    """nixpkgs' elan wraps the toolchain's own `lake` to export LEAN_CC pointing at a
    NixOS-aware compiler driver (`<toolchain>/bin/cc`); do the same for other tools."""
    env = dict(base)
    cc = stock_prefix / "bin" / "cc"
    if cc.exists():
        env["LEAN_CC"] = str(cc)
    return env


def in_dev_shell(cmd: list[str]) -> list[str]:
    """Run `cmd` inside the repository's `nix develop` shell if its tool is missing on NixOS."""
    if shutil.which(cmd[0]) is None and Path("/etc/NIXOS").exists() and shutil.which("nix"):
        return ["nix", "develop", str(ROOT), "--command", *cmd]
    return cmd


def build_flume(repo: Path, env: dict) -> Path:
    log(f"building flume in {repo}")
    logf = WORK / "logs" / f"cargo-{repo.name}.log"
    logf.parent.mkdir(parents=True, exist_ok=True)
    with open(logf, "w", encoding="utf-8") as out:
        p = subprocess.run(in_dev_shell(["cargo", "build", "--release", "--locked"]), cwd=repo, env=env,
                           stdout=out, stderr=subprocess.STDOUT)
    if p.returncode != 0:
        sys.exit(f"cargo build failed in {repo}; see {logf}")
    exe = repo / "target" / "release" / "flume"
    if not exe.exists():
        sys.exit(f"flume build in {repo} produced no {exe}")
    return exe


def build_blake(env: dict) -> tuple[Path, str]:
    """Blake at the pinned commit, built with its own `lake build`."""
    pin = json.loads(BLAKE_PIN.read_text(encoding="utf-8"))
    repo = WORK / "blake"
    if not repo.exists():
        run(["git", "clone", "-q", pin["repo"], str(repo)], env=env)
    if git("rev-parse", "HEAD", cwd=repo, check=False) != pin["commit"]:
        if subprocess.run(["git", "cat-file", "-e", f"{pin['commit']}^{{commit}}"], cwd=repo).returncode != 0:
            run(["git", "fetch", "-q", "origin"], cwd=repo, env=env)
        run(["git", "checkout", "-q", "--detach", pin["commit"]], cwd=repo, env=env)
    log(f"building Blake {pin['commit'][:7]} in {repo}")
    run(["lake", "build"], cwd=repo, env=env)
    exe = repo / ".lake" / "build" / "bin" / "lake"
    if not exe.exists():
        sys.exit(f"Blake build in {repo} produced no {exe}")
    return exe, pin["commit"]


def main_worktree(commit: str, env: dict) -> Path:
    wt = WORK / "worktrees" / "main"
    if wt.exists() and git("rev-parse", "HEAD", cwd=wt, check=False) != commit:
        run(["git", "worktree", "remove", "--force", str(wt)], cwd=ROOT, check=False)
        shutil.rmtree(wt, ignore_errors=True)
    if not wt.exists():
        wt.parent.mkdir(parents=True, exist_ok=True)
        run(["git", "worktree", "prune"], cwd=ROOT, check=False)
        run(["git", "worktree", "add", "--detach", str(wt), commit], cwd=ROOT)
    return wt


def resolve_main_ref(explicit: str | None) -> tuple[str, str]:
    if explicit:
        return git("rev-parse", explicit), explicit
    if git("rev-parse", "--verify", "-q", "origin/main", check=False):
        base = git("merge-base", "HEAD", "origin/main")
        return base, "merge-base(HEAD, origin/main)"
    if git("rev-parse", "--verify", "-q", "main", check=False):
        return git("merge-base", "HEAD", "main"), "merge-base(HEAD, main)"
    sys.exit("cannot find origin/main or main; pass --main-ref")


def server_support(exe: str, env: dict) -> bool:
    try:
        p = run([exe, "server", "protocol"], cwd=ROOT, env=env, check=False, timeout=60)
    except subprocess.TimeoutExpired:
        return False
    return p.returncode == 0 and SERVER_PROTOCOL in p.stdout


def resolve_tools(names: list[str], env: dict, main_ref: str | None, overrides: dict[str, str]) -> tuple[dict, dict]:
    stock_prefix = toolchain_prefix(env)
    tools: dict[str, dict] = {}
    head = git("rev-parse", "HEAD")
    dirty = tree_dirty()
    main_commit, main_how = resolve_main_ref(main_ref) if "main" in names else (None, None)
    tenv = with_lean_cc(env, stock_prefix)
    for name in names:
        if name in overrides:   # testing hook (makes the run non-standard)
            exe = Path(overrides[name]).resolve()
            tools[name] = {"exe": str(exe), "env": tenv, "override": True, "sha256": sha256_file(exe)}
        elif name == "lake":
            exe = stock_prefix / "bin" / "lake"
            if not exe.exists():
                sys.exit(f"stock lake not found at {exe}")
            if ROOT in exe.resolve().parents:
                sys.exit(f"stock lake {exe} resolves inside the flume checkout; refusing")
            tools[name] = {"exe": str(exe), "env": dict(env), "realpath": str(exe.resolve()),
                           "sha256": sha256_file(exe.resolve()),
                           "version": run([str(exe), "--version"], cwd=ROOT, env=env).stdout.strip()}
        elif name == "blake":
            exe, commit = build_blake(tenv)
            tools[name] = {"exe": str(exe), "env": tenv, "commit": commit, "sha256": sha256_file(exe),
                           "version": run([str(exe), "--version"], cwd=ROOT, env=tenv).stdout.strip()}
        elif name == "candidate":
            exe = build_flume(ROOT, env)
            tools[name] = {"exe": str(exe), "env": tenv, "commit": head, "dirty": dirty, "sha256": sha256_file(exe),
                           "version": run([str(exe), "--version"], cwd=ROOT, env=tenv).stdout.strip()}
        elif name == "main":
            if main_commit == head and not dirty and "candidate" in names and "candidate" not in overrides:
                log("main == candidate (clean HEAD at the baseline); measuring it once as `candidate`")
                tools[name] = {"alias": "candidate", "commit": main_commit, "ref": main_how}
                continue
            wt = main_worktree(main_commit, env)
            exe = build_flume(wt, env)
            tools[name] = {"exe": str(exe), "env": tenv, "commit": main_commit, "ref": main_how,
                           "sha256": sha256_file(exe),
                           "version": run([str(exe), "--version"], cwd=wt, env=tenv).stdout.strip()}
        else:
            sys.exit(f"unknown tool {name!r} (expected lake, blake, main, candidate)")
    for name, t in tools.items():
        if "exe" in t:
            t["server"] = server_support(t["exe"], t["env"])
    stock_lean = stock_prefix / "bin" / "lean"
    return tools, {"stock_prefix": str(stock_prefix),
                   "pin": (ROOT / "lean-toolchain").read_text(encoding="utf-8").strip(),
                   "lean_version": run([str(stock_lean), "--version"], cwd=ROOT, env=env).stdout.strip()}


# ---------------------------------------------------------------------------
# Corpora
# ---------------------------------------------------------------------------

def load_config(name: str) -> tuple[str, Path]:
    p = Path(name)
    if not p.exists():
        p = CONFIGS / f"{name}.json"
    if not p.exists():
        sys.exit(f"no such config: {name} (looked for {p})")
    return p.stem, p


def corpus_content_hash(src: Path) -> str:
    h = hashlib.sha256()
    for p in sorted((src / "Corpus").glob("*.lean")):
        h.update(p.read_bytes())
    return h.hexdigest()


def generator_version() -> int:
    return int(re.search(r"GENERATOR_VERSION = (\d+)", GEN.read_text(encoding="utf-8")).group(1))


def generate_corpus(cfg_name: str, cfg_path: Path, env: dict) -> Path:
    src = WORK / "corpora" / cfg_name / "src"
    meta = src / "corpus.json"
    if src.exists():
        ok = False
        if meta.exists():
            corpus = json.loads(meta.read_text(encoding="utf-8"))
            existing = corpus["config"]
            wanted = json.loads(cfg_path.read_text(encoding="utf-8"))
            ok = (all(existing.get(k) == v for k, v in wanted.items())
                  and existing.get("generator_version") == generator_version()
                  and "targets" in corpus
                  and corpus["stats"].get("content_sha256") == corpus_content_hash(src))
        if ok:
            return src
        log(f"corpus {cfg_name} is stale or damaged; regenerating")
        shutil.rmtree(src)
    log(f"generating corpus {cfg_name}")
    run([sys.executable, str(GEN), "--config", str(cfg_path), "--out", str(src)], env=env)
    return src


def config_scenarios(cfg: dict, scenarios: list[str]) -> list[str]:
    """The scenarios of this run that the corpus's config allows."""
    only = cfg.get("scenarios")
    excluded = set(cfg.get("exclude_scenarios", []))
    return [s for s in scenarios if (only is None or s in only) and s not in excluded]


def build_timeout(modules: int) -> int:
    return max(BUILD_TIMEOUT_S, int(modules * 0.25))


# ---------------------------------------------------------------------------
# Measurement
# ---------------------------------------------------------------------------

class ToolState:
    """Everything one tool keeps for one corpus: its cache directories and counters."""

    def __init__(self, name: str, tool: dict, corpus_root: Path, use_scope: bool, mem_gb: int | None,
                 extra_env: dict, timeout: int):
        self.name = name
        self.tool = tool
        self.use_scope = use_scope
        self.mem_gb = mem_gb
        self.timeout = timeout
        cache = corpus_root / f".bench-{name}"
        if cache.exists():
            shutil.rmtree(cache)
        for sub in ("lake", "xdg", "tmp"):
            (cache / sub).mkdir(parents=True)
        (cache / "lake-config.toml").write_text("", encoding="utf-8")
        self.cache = cache
        self.env = dict(tool["env"])
        self.env.update(extra_env)
        self.env.update({"LAKE_CACHE_DIR": str(cache / "lake"), "XDG_CACHE_HOME": str(cache / "xdg"),
                         "TMPDIR": str(cache / "tmp"), "LAKE_CONFIG": str(cache / "lake-config.toml"),
                         "LAKE_NO_CACHE": "1", "LAKE_ARTIFACT_CACHE": "true"})
        self.counter = 0
        self.stragglers = 0
        self.server_unit: str | None = None

    def unit(self, what: str) -> str:
        self.counter += 1
        return f"flume-bench-{os.getpid()}-{self.name}-{what}-{self.counter}"

    def scoped(self, unit: str, cmd: list[str]) -> list[str]:
        if not self.use_scope:
            return ["bash", str(SCOPE_STATS), *cmd]
        props = ["-p", f"MemoryMax={self.mem_gb}G"] if self.mem_gb else []
        return ["systemd-run", "--user", "--scope", "-q", f"--unit={unit}", *props, "--",
                "bash", str(SCOPE_STATS), *cmd]

    def reap(self, unit: str) -> int:
        """Kill anything a scope left behind; return how many processes there were.

        systemd notices that a scope's cgroup emptied asynchronously, so a scope whose
        command exits within milliseconds can still read `active` for a moment; only
        processes that remain in it for a second count as stragglers."""
        if not self.use_scope:
            return 0
        n = 0
        deadline = time.monotonic() + 1.0
        while True:
            show = subprocess.run(["systemctl", "--user", "show", "-p", "ActiveState", "-p", "NPids", f"{unit}.scope"],
                                  capture_output=True, text=True)
            props = dict(line.split("=", 1) for line in show.stdout.splitlines() if "=" in line)
            if props.get("ActiveState") not in ("active", "deactivating"):
                n = 0
                break
            n = int(props.get("NPids", "0") or 0)
            if n == 0 or time.monotonic() >= deadline:
                break
            time.sleep(0.02)
        if props.get("ActiveState") in ("active", "deactivating"):
            subprocess.run(["systemctl", "--user", "stop", f"{unit}.scope"], capture_output=True)
        self.stragglers += n
        return n

    @staticmethod
    def read_stats(path: Path) -> dict:
        stats: dict = {}
        if not path.exists():
            return stats
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.startswith("memory_peak="):
                stats["memory_peak"] = int(line.split("=", 1)[1])
            elif line.startswith(("usage_usec ", "user_usec ", "system_usec ")):
                k, v = line.split()
                stats[k] = int(v)
        return stats

    def _measure(self, unit: str, cmd: list[str], cwd: Path) -> tuple[str, int, bool, float, dict, int, float]:
        stats_file = self.cache / f"stats-{unit}.txt"
        stats_file.unlink(missing_ok=True)
        env = dict(self.env)
        env["FLUME_BENCH_STATS"] = str(stats_file)
        env["FLUME_BENCH_SCOPED"] = "1" if self.use_scope else "0"
        before = resource.getrusage(resource.RUSAGE_CHILDREN)
        t0 = time.perf_counter()
        timed_out = False
        try:
            proc = subprocess.run(self.scoped(unit, cmd), cwd=cwd, env=env, stdout=subprocess.PIPE,
                                  stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace",
                                  timeout=self.timeout, start_new_session=True)
            out, code = proc.stdout, proc.returncode
        except subprocess.TimeoutExpired as e:
            raw = e.stdout or ""
            out = raw.decode("utf-8", "replace") if isinstance(raw, bytes) else raw
            code, timed_out = -1, True
        wall = time.perf_counter() - t0
        after = resource.getrusage(resource.RUSAGE_CHILDREN)
        stragglers = self.reap(unit)
        stats = self.read_stats(stats_file)
        stats_file.unlink(missing_ok=True)
        stats.setdefault("user_usec", int((after.ru_utime - before.ru_utime) * 1e6))
        stats.setdefault("system_usec", int((after.ru_stime - before.ru_stime) * 1e6))
        return out, code, timed_out, wall, stats, stragglers, after.ru_maxrss

    def build(self, checkout: Path) -> dict:
        unit = self.unit("build")
        out, code, timed_out, wall, stats, stragglers, maxrss = self._measure(unit, [self.tool["exe"], "build"], checkout)
        return {
            "wall_s": round(wall, 4),
            "cpu_user_s": round(stats["user_usec"] / 1e6, 3),
            "cpu_sys_s": round(stats["system_usec"] / 1e6, 3),
            "peak_mem_mb": round(stats["memory_peak"] / (1024 * 1024), 1) if "memory_peak" in stats else None,
            "exit_code": code,
            "timed_out": timed_out,
            "stragglers": stragglers,
            "jobs": len(re.findall(r"^✔", out, flags=re.M)),
            "failed_jobs": len(re.findall(r"^✖", out, flags=re.M)),
            "log_tail": out[-3000:] if code != 0 else "",
        }

    def build_many(self, checkouts: list[Path]) -> dict:
        """Build several checkouts at once inside one scope; wall_s is the time until all are done."""
        unit = self.unit("agents")
        report = self.cache / f"multi-{unit}.json"
        cmd = [sys.executable, str(MULTI_BUILD), str(report), self.tool["exe"], *map(str, checkouts)]
        out, code, timed_out, wall, stats, stragglers, _ = self._measure(unit, cmd, checkouts[0].parent)
        rep = json.loads(report.read_text(encoding="utf-8")) if report.exists() else {"builds": [], "makespan_s": wall}
        report.unlink(missing_ok=True)
        builds = []
        for b in rep["builds"]:
            log_path = Path(b["dir"]) / ".bench-build.log"
            text = log_path.read_text(encoding="utf-8", errors="replace") if log_path.exists() else ""
            log_path.unlink(missing_ok=True)
            builds.append({"wall_s": b["wall_s"], "exit_code": b["exit_code"],
                           "jobs": len(re.findall(r"^✔", text, flags=re.M)),
                           "failed_jobs": len(re.findall(r"^✖", text, flags=re.M)),
                           "log_tail": text[-2000:] if b["exit_code"] != 0 else ""})
        ok = code == 0 and builds and all(b["exit_code"] == 0 for b in builds)
        return {
            "wall_s": round(rep.get("makespan_s", wall), 4),
            "harness_wall_s": round(wall, 4),
            "cpu_user_s": round(stats["user_usec"] / 1e6, 3),
            "cpu_sys_s": round(stats["system_usec"] / 1e6, 3),
            "peak_mem_mb": round(stats["memory_peak"] / (1024 * 1024), 1) if "memory_peak" in stats else None,
            "exit_code": 0 if ok else (code or 1),
            "timed_out": timed_out,
            "stragglers": stragglers,
            "builds": builds,
            "jobs": sum(b["jobs"] for b in builds),
            "failed_jobs": sum(b["failed_jobs"] for b in builds),
            "log_tail": out[-2000:] if not ok else "",
        }

    def start_server(self, checkout: Path) -> bool:
        unit = f"flume-bench-{os.getpid()}-{self.name}-server"
        props = ["-p", f"MemoryMax={self.mem_gb}G"] if (self.use_scope and self.mem_gb) else []
        cmd = ([ "systemd-run", "--user", "--scope", "-q", f"--unit={unit}", *props, "--"] if self.use_scope else []) \
            + [self.tool["exe"], "server", "start"]
        p = subprocess.run(cmd, cwd=checkout, env=self.env, capture_output=True, text=True, timeout=600,
                           start_new_session=True)
        self.server_unit = unit if self.use_scope else None
        if p.returncode != 0:
            log(f"[{self.name}] server start failed ({p.returncode}): {(p.stdout + p.stderr)[-1000:]}")
            return False
        return True

    def stop_server(self, checkout: Path) -> int:
        subprocess.run([self.tool["exe"], "server", "stop"], cwd=checkout, env=self.env, capture_output=True,
                       timeout=600)
        time.sleep(0.5)
        n = self.reap(self.server_unit) if self.server_unit else 0
        self.server_unit = None
        return n


class Hasher:
    """sha256 of files, remembered by (path, size, mtime, inode) so unchanged artifacts are hashed once."""

    def __init__(self) -> None:
        self.memo: dict[tuple, str] = {}

    def __call__(self, p: Path) -> str:
        st = p.stat()
        key = (str(p), st.st_size, st.st_mtime_ns, st.st_ino)
        h = self.memo.get(key)
        if h is None:
            h = sha256_file(p)
            self.memo[key] = h
        return h


def facet_path(build: Path, mod: str, facet: str) -> Path:
    rel = mod.replace(".", "/")
    if facet == "c":
        return build / "ir" / f"{rel}.c"
    return build / "lib" / "lean" / f"{rel}.{facet}"


def artifact_manifest(state: ToolState, checkout: Path, modules: list[str], facets: tuple[str, ...],
                      hasher: Hasher) -> dict[str, str]:
    """{module:facet: sha256} of every artifact; ones missing from .lake/build are located with `lake query`."""
    build = checkout / ".lake" / "build"
    manifest: dict[str, str] = {}
    missing: list[str] = []
    for mod in modules:
        for facet in facets:
            p = facet_path(build, mod, facet)
            if p.is_file():
                manifest[f"{mod}:{facet}"] = hasher(p)
            else:
                missing.append(f"{mod}:{facet}")
    for i in range(0, len(missing), 2000):
        batch = missing[i:i + 2000]
        try:
            p = subprocess.run([state.tool["exe"], "query", *batch], cwd=checkout, env=state.env,
                               capture_output=True, text=True, timeout=state.timeout)
        except subprocess.TimeoutExpired:
            continue
        lines = [line.strip() for line in p.stdout.splitlines() if line.strip()]
        if p.returncode != 0 or len(lines) != len(batch):
            continue
        for target, path in zip(batch, lines):
            q = Path(path)
            if q.is_file():
                manifest[target] = hasher(q)
    return manifest


def compare_manifests(ref: dict[str, str], other: dict[str, str], expected: int) -> dict:
    missing = sorted(set(ref) - set(other))
    extra = sorted(set(other) - set(ref))
    differ = sorted(k for k in ref.keys() & other.keys() if ref[k] != other[k])
    complete = len(ref) == expected
    return {"identical": complete and not (missing or extra or differ), "files_compared": len(ref),
            "files_expected": expected, "n_missing": len(missing), "n_extra": len(extra), "n_differ": len(differ),
            "missing": missing[:20], "extra": extra[:20], "differ": differ[:20]}


def summarize(runs: list[dict]) -> dict:
    walls = [r["wall_s"] for r in runs if r["exit_code"] == 0]
    out = {"runs": runs, "ok": bool(runs) and all(r["exit_code"] == 0 and not r["stragglers"] for r in runs)}
    if walls:
        out["median_wall_s"] = round(statistics.median(walls), 4)
        out["min_wall_s"] = round(min(walls), 4)
        out["max_wall_s"] = round(max(walls), 4)
    return out


class CorpusRun:
    """Measures every tool on one corpus. Tools are interleaved run by run unless `sequential`."""

    def __init__(self, cfg_name: str, src: Path, corpus: dict, tools: dict, order: list[str], scenarios: list[str],
                 repeat: int, repeat_fast: int, use_scope: bool, mem_gb: int | None, extra_env: dict,
                 break_tools: set[str]):
        self.cfg_name = cfg_name
        self.src = src
        self.corpus = corpus
        self.targets = corpus["targets"]
        self.order = order
        self.scenarios = scenarios
        self.repeat = repeat
        self.repeat_fast = repeat_fast
        self.break_tools = break_tools
        self.root = WORK / "corpora" / cfg_name
        self.facets = MODULE_FACETS if corpus["config"].get("module_system") else LEGACY_FACETS
        timeout = build_timeout(corpus["stats"]["modules"])
        self.states = {n: ToolState(n, tools[n], self.root, use_scope, mem_gb, extra_env, timeout) for n in order}
        self.checkout = {n: self.root / n for n in order}
        self.modules = {n: [m["name"] for m in corpus["modules"]] for n in order}
        self.results: dict[str, dict] = {n: {} for n in order}
        self.manifests: dict[str, dict] = {n: {} for n in order}
        self.failed: set[str] = set()
        self.hasher = Hasher()
        self.stamp = 0

    # -- helpers -------------------------------------------------------------
    def record(self, name: str, step: str, checkout: Path, modules: list[str]) -> None:
        self.manifests[name][step] = artifact_manifest(self.states[name], checkout, modules, self.facets, self.hasher)

    def note_failure(self, name: str, what: str, r: dict) -> None:
        if r["exit_code"] != 0 or r["stragglers"]:
            log(f"[{self.cfg_name}/{name}] {what} FAILED (exit {r['exit_code']}, stragglers {r['stragglers']})"
                f"{' (timed out)' if r.get('timed_out') else ''}")
            self.failed.add(name)

    def rotation(self, i: int) -> list[str]:
        order = self.order[i % len(self.order):] + self.order[: i % len(self.order)]
        if i % 2:
            order.reverse()  # a rotated cycle never reverses; alternate it so position effects cancel
        return [n for n in order if n not in self.failed]

    def report(self, name: str, key: str, runs: list[dict]) -> None:
        res = self.results[name][key]
        if res.get("median_wall_s") is not None:
            log(f"[{self.cfg_name}/{name}] {key}: median {res['median_wall_s']:.2f}s "
                f"(min {res['min_wall_s']:.2f}, max {res['max_wall_s']:.2f}, n {len(runs)}, jobs {runs[0]['jobs']})")

    def edits_for(self, scen: str, i: int) -> list[tuple[str, str]]:
        t = self.targets
        if scen == "leaf":
            return [(t["leaf"], edits.KINDS[i % len(edits.KINDS)])]
        if scen == "hub-private":
            return [(t["hub"], edits.PRIVATE_KINDS[i % len(edits.PRIVATE_KINDS)])]
        if scen == "hub-interface":
            return [(t["hub"], edits.INTERFACE_KINDS[i % len(edits.INTERFACE_KINDS)])]
        if scen == "swarm":
            return [(m, edits.KINDS[(k + i) % len(edits.KINDS)]) for k, m in enumerate(t["swarm"])]
        return []

    def apply(self, name: str, scen: str, i: int, checkout: Path) -> None:
        for mod, kind in self.edits_for(scen, i):
            edits.apply_edit(checkout, self.corpus, mod, kind, self.stamp)
        if scen == "newfile":
            new = edits.new_module(checkout, self.corpus, self.stamp, [self.targets["leaf"], self.targets["hub"]])
            if new not in self.modules[name]:
                self.modules[name].append(new)

    # -- phases --------------------------------------------------------------
    def cold(self, names: list[str]) -> None:
        for name in names:
            copy_tree(self.src, self.checkout[name], with_build=False)
            log(f"[{self.cfg_name}/{name}] cold build")
            r = self.states[name].build(self.checkout[name])
            self.results[name]["cold"] = summarize([r])
            self.record(name, "cold", self.checkout[name], self.modules[name])
            self.note_failure(name, "cold build", r)
            if name not in self.failed:
                log(f"[{self.cfg_name}/{name}] cold: {r['wall_s']:.1f}s wall, {r['cpu_user_s'] + r['cpu_sys_s']:.1f}s cpu, "
                    f"peak mem {r['peak_mem_mb'] or '?'} MB, {r['jobs']} jobs")

    def scenario(self, scen: str, key: str, names: list[str]) -> None:
        """Run one scenario for `names`, paired and interleaved (or one after another if len(names) == 1)."""
        runs: dict[str, list[dict]] = {n: [] for n in names}
        n_runs, fast, i = self.repeat, False, 0
        agents: dict[str, list[Path]] = {}
        if scen in AGENTS:
            k = min(AGENTS[scen], len(self.targets["agents"]))
            for name in names:
                if name in self.failed:
                    continue
                agents[name] = []
                for a in range(k):
                    d = self.root / f"{name}-agent{a}"
                    copy_tree(self.checkout[name], d, with_build=True)
                    agents[name].append(d)
        while i < n_runs:
            self.stamp += 1
            for name in (self.rotation(i) if len(names) > 1 else [n for n in names if n not in self.failed]):
                if name not in names:
                    continue
                if scen == "clone":
                    d = self.root / f"{name}-clone"
                    copy_tree(self.checkout[name], d, with_build=False)
                    r = self.states[name].build(d)
                    self.record(name, f"{key}/{i}", d, self.modules[name])
                    shutil.rmtree(d, ignore_errors=True)
                elif scen in AGENTS:
                    for a, d in enumerate(agents[name]):
                        mod = self.targets["agents"][a]
                        edits.apply_edit(d, self.corpus, mod, edits.KINDS[(a + i) % len(edits.KINDS)], self.stamp)
                    r = self.states[name].build_many(agents[name])
                    for a, d in enumerate(agents[name]):
                        self.record(name, f"{key}/{i}/agent{a}", d, self.modules[name])
                else:
                    self.apply(name, scen, i, self.checkout[name])
                    r = self.states[name].build(self.checkout[name])
                    self.record(name, f"{key}/{i}", self.checkout[name], self.modules[name])
                runs[name].append(r)
                self.note_failure(name, f"{key} run {i + 1}", r)
            if i == 0:
                first = [runs[n][0]["wall_s"] for n in names if runs[n]]
                if first and max(first) < FAST_SECONDS:
                    fast = True
                    n_runs = max(self.repeat, self.repeat_fast)  # cheap scenario: take many samples
            i += 1
        for name in names:
            self.results[name][key] = summarize(runs[name])
            self.results[name][key]["fast"] = fast
            if runs[name]:
                self.report(name, key, runs[name])
        for dirs in agents.values():
            for d in dirs:
                shutil.rmtree(d, ignore_errors=True)

    def breaks(self, names: list[str]) -> None:
        for name in names:
            if name not in self.break_tools:
                continue
            if name in self.failed:
                self.results[name]["break"] = {"ok": False, "skipped": "earlier failure"}
                continue
            s, d = self.states[name], self.checkout[name]
            brk: dict = {"ok": True}
            for kind in ("rename", "body"):
                original = edits.apply_break(d, self.corpus, self.targets["hub"], kind)
                broken = s.build(d)
                edits.module_file(d, self.corpus, self.targets["hub"]).write_text(original, encoding="utf-8")
                recovered = s.build(d)
                self.record(name, f"recovered/{kind}", d, self.modules[name])
                brk[kind] = {"broken_exit_code": broken["exit_code"], "broken_failed_jobs": broken["failed_jobs"],
                             "recovered_exit_code": recovered["exit_code"], "recovered_wall_s": recovered["wall_s"]}
                brk["ok"] = brk["ok"] and broken["exit_code"] != 0 and recovered["exit_code"] == 0 \
                    and not (broken["stragglers"] or recovered["stragglers"])
                log(f"[{self.cfg_name}/{name}] break/{kind}: broken build exit {broken['exit_code']} "
                    f"({broken['failed_jobs']} failed jobs), recovered exit {recovered['exit_code']}")
            self.results[name]["break"] = brk

    def with_servers(self, names: list[str], fn) -> None:
        """Run fn() with the servers of the server-capable tools among `names` warm."""
        started = []
        for name in names:
            if self.states[name].tool.get("server") and name not in self.failed:
                if self.states[name].start_server(self.checkout[name]):
                    started.append(name)
                    self.states[name].build(self.checkout[name])  # warm-up, unmeasured
                else:
                    self.failed.add(name)
        try:
            fn()
        finally:
            for name in started:
                n = self.states[name].stop_server(self.checkout[name])
                self.results[name]["server_stragglers"] = n
                if n:
                    log(f"[{self.cfg_name}/{name}] server left {n} processes behind")
                    self.failed.add(name)

    def run(self, sequential: bool) -> None:
        groups = [[n] for n in self.order] if sequential else [list(self.order)]
        any_server = any(self.states[n].tool.get("server") for n in self.order)
        for names in groups:
            self.cold(names)
            for scen in self.scenarios:
                if scen != "cold":
                    self.scenario(scen, scen, names)
            if any_server:
                def warm(names=names):
                    for scen in self.scenarios:
                        if scen != "cold":
                            self.scenario(scen, f"{scen}@warm", names)
                self.with_servers(names, warm)
            self.breaks(names)
            for name in names:
                self.results[name]["stragglers_total"] = self.states[name].stragglers
            if sequential:   # free the disk before the next tool
                for name in names:
                    shutil.rmtree(self.checkout[name], ignore_errors=True)
                    shutil.rmtree(self.states[name].cache, ignore_errors=True)
            if sequential and len(self.order) > 1:
                self.stamp = 0   # the next tool sees exactly the same sequence of edits

    def cleanup(self) -> None:
        for name in self.order:
            shutil.rmtree(self.checkout[name], ignore_errors=True)
            shutil.rmtree(self.states[name].cache, ignore_errors=True)


# ---------------------------------------------------------------------------
# Verdict and reporting
# ---------------------------------------------------------------------------

def scenario_keys(measurements: dict) -> list[str]:
    keys: list[str] = []
    for per_tool in measurements.values():
        for res in per_tool.values():
            for k in res:
                base = k.split("@")[0]
                if base in SCENARIOS and k not in keys:
                    keys.append(k)
    return sorted(keys, key=lambda k: (k.split("@")[1] if "@" in k else "", SCENARIOS.index(k.split("@")[0])))


def correctness_for(tool_name: str, manifests: dict, measurements: dict, corpora: dict) -> dict:
    """Compare a tool's artifacts with lake's at every step, plus build health."""
    out: dict = {}
    for cfg_name, per_tool in manifests.items():
        lake_m = per_tool.get("lake", {})
        mine = per_tool.get(tool_name, {})
        steps = {step: compare_manifests(ref, mine.get(step, {}), corpora[cfg_name]["expected_files"].get(step, -1))
                 for step, ref in lake_m.items()}
        res = measurements[cfg_name].get(tool_name, {})
        lake_res = measurements[cfg_name].get("lake", {})
        builds_ok = all(v.get("ok", False) for k, v in res.items() if k.split("@")[0] in SCENARIOS) \
            and not res.get("server_stragglers")
        brk, lake_brk = res.get("break"), lake_res.get("break", {})
        break_ok = None if brk is None else (bool(brk.get("ok")) and all(
            brk.get(kind, {}).get("broken_failed_jobs") == lake_brk.get(kind, {}).get("broken_failed_jobs")
            for kind in ("rename", "body")))
        out[cfg_name] = {
            "identical": bool(steps) and all(v["identical"] for v in steps.values()),
            "builds_ok": builds_ok,
            "break_ok": break_ok,
            "stragglers": res.get("stragglers_total", 0),
            "steps_compared": len(steps),
            "steps_differing": {k: v for k, v in steps.items() if not v["identical"]},
        }
    return out


def correct(c: dict) -> bool:
    return bool(c) and all(v["identical"] and v["builds_ok"] and v["break_ok"] is not False for v in c.values())


def verdicts(measurements: dict, correctness: dict, tools: list[str], targets: list[str] | None = None,
             sequential: bool = False) -> dict:
    out: dict = {"per_scenario": {}, "improved": [], "regressed": [], "overall": "n/a", "reasons": [],
                 "targets": targets or []}
    if "candidate" not in tools:
        return out
    incorrect = []
    for cfg_name, c in correctness.get("candidate", {}).items():
        if not c["identical"]:
            incorrect.append(f"{cfg_name}: artifacts differ from lake")
        if not c["builds_ok"]:
            incorrect.append(f"{cfg_name}: a build failed or left processes behind")
        if c["break_ok"] is False:
            incorrect.append(f"{cfg_name}: breaking-edit check failed")
    out["reasons"] = incorrect
    if "main" not in tools:
        out["overall"] = "failure" if incorrect else "n/a (no main measured)"
        return out
    for cfg_name, per_tool in measurements.items():
        for key in scenario_keys({cfg_name: per_tool}):
            m = per_tool.get("main", {}).get(key, {})
            c = per_tool.get("candidate", {}).get(key, {})
            if m.get("median_wall_s") is None or c.get("median_wall_s") is None:
                continue
            m_ok = [x["wall_s"] for x in m["runs"] if x["exit_code"] == 0]
            c_ok = [x["wall_s"] for x in c["runs"] if x["exit_code"] == 0]
            if sequential:
                t, stat = SEQUENTIAL_THRESHOLD, "unpaired medians"
                ratio, n = m["median_wall_s"] / c["median_wall_s"], min(len(m_ok), len(c_ok))
            else:
                if key.split("@")[0] == "cold":
                    t = COLD_THRESHOLD
                elif m.get("fast", m["median_wall_s"] < FAST_SECONDS):
                    t = NOISE_FLOOR_FAST
                else:
                    t = NOISE_FLOOR
                n = min(len(m_ok), len(c_ok))
                ratio, stat = statistics.median(m_ok[i] / c_ok[i] for i in range(n)), "paired"
            state = "improved" if ratio >= 1 + t else ("regressed" if ratio <= 1 - t else "same")
            out["per_scenario"][f"{cfg_name}/{key}"] = {"statistic": stat, "pairs": n, "main_s": m["median_wall_s"],
                                                         "candidate_s": c["median_wall_s"], "speedup": round(ratio, 4),
                                                         "threshold": round(t, 4), "state": state}
            if state == "improved":
                out["improved"].append(f"{cfg_name}/{key}")
            elif state == "regressed":
                out["regressed"].append(f"{cfg_name}/{key}")
    wanted = set(targets or SCENARIOS)
    target_improved = [k for k in out["improved"] if k.split("/", 1)[1].split("@")[0] in wanted]
    if incorrect or out["regressed"]:
        out["overall"] = "failure"
    elif target_improved:
        out["overall"] = "success"
    else:
        out["overall"] = "neutral"
    return out


def verdict_for_result(result: dict) -> dict:
    """The verdict of a stored result under this harness's rule (used by --rescore and the PR check)."""
    return verdicts(result["measurements"], result["correctness"], result["tool_order"],
                    result.get("targets") or None, bool(result.get("sequential")))


def ratios(measurements: dict, tools: list[str]) -> dict:
    """speedup[tool][vs_base][scenario] = base_median / tool_median, geometric mean over corpora;
    plus per-workflow geometric means and `overall`, the geometric mean of the three workflows."""
    out: dict = {}
    keys = scenario_keys(measurements)
    for tool in tools:
        if tool == "lake":
            continue
        out[tool] = {}
        for base in ("lake", "main"):
            if base not in tools or base == tool:
                continue
            per_key: dict[str, list[float]] = {}
            for per_tool in measurements.values():
                for key in keys:
                    a = per_tool.get(base, {}).get(key, {}).get("median_wall_s")
                    b = per_tool.get(tool, {}).get(key, {}).get("median_wall_s")
                    if a and b:
                        per_key.setdefault(key, []).append(a / b)
            sp = {k: round(statistics.geometric_mean(v), 4) for k, v in per_key.items()}
            flows = {}
            for flow, scens in WORKFLOWS.items():
                vals = [sp[s] for s in scens if s in sp]
                if vals:
                    flows[flow] = round(statistics.geometric_mean(vals), 4)
            if flows:
                sp.update({f"workflow:{k}": v for k, v in flows.items()})
                sp["overall"] = round(statistics.geometric_mean(list(flows.values())), 4)
            out[tool][f"vs_{base}"] = sp
    return out


def print_report(result: dict) -> None:
    tools = result["tool_order"]
    print()
    print(f"{'corpus':<16}{'scenario':<20}" + "".join(f"{t:>12}" for t in tools) + "   (median wall s; FAIL = build failed)")
    for cfg_name, per_tool in result["measurements"].items():
        for key in scenario_keys({cfg_name: per_tool}):
            row = []
            for t in tools:
                s = per_tool.get(t, {}).get(key, {})
                if s.get("median_wall_s") is not None and s.get("ok"):
                    row.append(f"{s['median_wall_s']:>12.2f}")
                elif s:
                    row.append(f"{'FAIL':>12}")
                else:
                    row.append(f"{'-':>12}")
            print(f"{cfg_name:<16}{key:<20}" + "".join(row))
    print()
    for tool, r in result["speedup"].items():
        for base, per in r.items():
            print(f"speedup of {tool} {base.replace('_', ' ')}: " + ", ".join(f"{k}={v:.3f}x" for k, v in per.items()))
    for tool, c in result["correctness"].items():
        good = correct(c)
        print(f"correctness of {tool}: " + ("identical artifacts to lake, all builds ok, break check ok" if good else
              "INCORRECT: " + "; ".join(f"{k}: identical={v['identical']} builds_ok={v['builds_ok']} break_ok={v['break_ok']}"
                                         for k, v in c.items()
                                         if not (v["identical"] and v["builds_ok"] and v["break_ok"] is not False))))
    v = result["verdict"]
    for key, s in v.get("per_scenario", {}).items():
        print(f"verdict {key}: {s['state']} ({s['statistic']} speedup {s['speedup']:.3f}x over {s['pairs']} runs, "
              f"threshold {s['threshold'] * 100:.0f}%; medians main {s['main_s']:.2f}s, candidate {s['candidate_s']:.2f}s)")
    tgt = f" [targets: {', '.join(v['targets'])}]" if v.get("targets") else ""
    print(f"verdict: {v['overall']}{tgt}" + (f" ({'; '.join(v['reasons'])})" if v.get("reasons") else ""))
    print(f"standard run: {'yes' if result['standard'] else 'no (' + ', '.join(result['nonstandard_reasons']) + ')'}"
          f" (suite {result.get('suite')})")
    print()


def rescore(path: Path) -> None:
    """Recompute speedup and verdict of a stored result from its raw runs."""
    result = json.loads(path.read_text(encoding="utf-8"))
    if result.get("schema", 0) < 3:
        sys.exit(f"{path} has schema {result.get('schema')}; this harness rescores schema 3 results only")
    previous = result.get("verdict", {}).get("overall")
    result["speedup"] = ratios(result["measurements"], result["tool_order"])
    result["verdict"] = verdict_for_result(result)
    result.setdefault("rescored", []).append({
        "at": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "run_py_sha256": sha256_file(Path(__file__)), "noise_rule": NOISE_RULE, "previous_verdict": previous})
    result["noise_rule"] = NOISE_RULE
    path.write_text(json.dumps(result, indent=1) + "\n", encoding="utf-8")
    print_report(result)
    log(f"rescored {path}: {previous} -> {result['verdict']['overall']}")


def suite_configs(suite: dict) -> list[str]:
    names: list[str] = []
    for pattern in suite["configs"]:
        names += sorted(p.stem for p in CONFIGS.glob(f"{pattern}.json"))
    return names


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--suite", default="standard", choices=sorted(SUITES), help="corpora, scenarios, and repetitions")
    ap.add_argument("--quick", action="store_true", help="alias for --suite quick")
    ap.add_argument("--configs", default=None, help="comma-separated config names or paths (overrides the suite's)")
    ap.add_argument("--tools", default=None, help="comma-separated: lake, blake, main, candidate (default: the suite's)")
    ap.add_argument("--main-ref", default=None, help="git ref for `main` (default: merge-base of HEAD and origin/main)")
    ap.add_argument("--scenarios", default=None, help=f"comma-separated subset of {','.join(SCENARIOS)}")
    ap.add_argument("--targets", default="", help="scenarios a performance milestone targets (verdict `success` needs one improved)")
    ap.add_argument("--repeat", type=int, default=None, help="paired runs per scenario taking >= 2 s (suite default)")
    ap.add_argument("--repeat-fast", type=int, default=None, help="paired runs per scenario taking < 2 s (suite default)")
    ap.add_argument("--cpus", default=None, help="CPU list to pin to, e.g. 0-15 (default: bench/local.json or half)")
    ap.add_argument("--mem-gb", type=int, default=None, help="memory cap in GiB (default: bench/local.json or half)")
    ap.add_argument("--out", default=None, help="result JSON path (default: research/results/ for standard runs, else bench/work/results/)")
    ap.add_argument("--label", default="", help="free-form label stored in the result (e.g. the milestone's slug)")
    ap.add_argument("--keep", action="store_true", help="keep per-tool corpus copies after the run")
    ap.add_argument("--tool-exe", action="append", default=[], metavar="NAME=PATH",
                    help="testing hook: use PATH as tool NAME (non-standard)")
    ap.add_argument("--rescore", metavar="FILE", help="recompute the verdict of an existing result file under the current rule and exit")
    args = ap.parse_args()
    if args.rescore:
        rescore(Path(args.rescore))
        return
    suite_name = "quick" if args.quick else args.suite
    suite = SUITES[suite_name]
    repeat = suite["repeat"] if args.repeat is None else args.repeat
    repeat_fast = suite["repeat_fast"] if args.repeat_fast is None else args.repeat_fast
    targets = [t for t in args.targets.split(",") if t]
    for t in targets:
        if t not in SCENARIOS:
            sys.exit(f"unknown target scenario {t}")

    caps = default_caps()
    if args.cpus:
        caps["cpus"] = args.cpus
    if args.mem_gb is not None:
        caps["mem_gb"] = args.mem_gb
    requested = parse_cpus(caps["cpus"])
    cpus, affinity_ok = apply_cpu_cap(requested)
    if caps["mem_gb"] and total_mem_gb() and caps["mem_gb"] > total_mem_gb():
        sys.exit(f"--mem-gb {caps['mem_gb']} exceeds this machine's {total_mem_gb()} GB")
    use_scope, mem_mode = scope_support(caps["mem_gb"])
    if not affinity_ok:
        banner("CPU cap is NOT enforced on this platform; results will not be standard")
    if mem_mode.startswith("unenforced"):
        banner(f"memory cap is NOT enforced ({mem_mode}); the machine may be oversubscribed")
    extra_env = {"LEAN_NUM_THREADS": str(len(cpus))}
    cpu = cpu_model()
    machine = {"id": machine_id(cpu), "os": f"{platform.system()} {platform.release()}", "arch": platform.machine(),
               "cpu": cpu, "cpus_total": os.cpu_count(), "mem_total_gb": total_mem_gb(),
               "caps": {"cpus": ",".join(str(c) for c in cpus), "cpu_count": len(cpus), "cpu_affinity_enforced": affinity_ok,
                        "mem_gb": caps["mem_gb"], "mem_enforcement": mem_mode, "process_scopes": use_scope}}
    log(f"suite {suite_name}; caps: cpus={machine['caps']['cpus']} ({len(cpus)}), affinity={'on' if affinity_ok else 'OFF'}, "
        f"mem={caps['mem_gb']}G via {mem_mode}, scopes={'on' if use_scope else 'off'}, LEAN_NUM_THREADS={len(cpus)}")

    default_cfgs = suite_configs(suite)
    cfg_names = [c for c in args.configs.split(",") if c] if args.configs else default_cfgs
    if not cfg_names:
        sys.exit("no configs selected")
    default_scenarios = [s for s in SCENARIOS if s not in suite["exclude"]]
    scenarios = [s for s in args.scenarios.split(",") if s] if args.scenarios else default_scenarios
    for s in scenarios:
        if s not in SCENARIOS:
            sys.exit(f"unknown scenario {s}")
    if "cold" not in scenarios:
        scenarios = ["cold", *scenarios]   # every corpus starts with a cold build
    scenarios = [s for s in SCENARIOS if s in scenarios]

    env, scrubbed = scrubbed_env()
    if scrubbed:
        log(f"scrubbed from the environment: {', '.join(scrubbed)}")
    tool_names = [t for t in (args.tools or suite["tools"]).split(",") if t]
    overrides = dict(x.split("=", 1) for x in args.tool_exe)
    # The tree's state when the candidate is built is what the result describes.
    dirty = tree_dirty()
    head_commit = git("rev-parse", "HEAD")
    head_branch = git("rev-parse", "--abbrev-ref", "HEAD")
    tools, toolchain = resolve_tools(tool_names, env, args.main_ref, overrides)
    tool_order = [t for t in tool_names if "alias" not in tools[t]]
    break_tools = {"lake", "candidate"} if "candidate" in tool_order else set(tool_order)

    nonstandard = []
    if args.main_ref:
        nonstandard.append("main-ref")
    if cfg_names != default_cfgs:
        nonstandard.append("configs")
    if tool_names != suite["tools"].split(","):
        nonstandard.append("tools")
    if scenarios != [s for s in SCENARIOS if s in default_scenarios]:
        nonstandard.append("scenarios")
    if repeat < suite["repeat"] or repeat_fast < suite["repeat_fast"]:
        nonstandard.append("fewer repetitions than the suite's")
    if suite_name == "quick":
        nonstandard.append("quick suite")
    if overrides:
        nonstandard.append("tool override")
    if dirty:
        nonstandard.append("dirty tree")
    if not affinity_ok:
        nonstandard.append("cpu cap unenforced")
    standard = not nonstandard

    measurements: dict = {}
    manifests_all: dict = {}
    corpora_meta: dict = {}
    orders: dict = {}
    t_start = time.time()
    for idx, name in enumerate(cfg_names):
        cfg_name, cfg_path = load_config(name)
        src = generate_corpus(cfg_name, cfg_path, env)
        corpus = json.loads((src / "corpus.json").read_text(encoding="utf-8"))
        scens = config_scenarios(corpus["config"], scenarios)
        log(f"corpus {cfg_name}: {corpus['stats']['modules']} modules, {corpus['stats']['lines']} lines, "
            f"{'module system' if corpus['config'].get('module_system') else 'no module system'}, hub "
            f"{corpus['targets']['hub']} ({corpus['targets']['hub_transitive_dependents']} transitive dependents), "
            f"scenarios {','.join(scens)}")
        # rotate the tool order per corpus (and per repetition inside CorpusRun)
        order = tool_order[idx % len(tool_order):] + tool_order[: idx % len(tool_order)]
        if suite["sequential"] and "lake" in order:
            order = ["lake"] + [t for t in order if t != "lake"]
        orders[cfg_name] = order
        cr = CorpusRun(cfg_name, src, corpus, tools, order, scens, repeat, repeat_fast, use_scope, caps["mem_gb"],
                       extra_env, break_tools)
        cr.run(suite["sequential"])
        measurements[cfg_name], manifests_all[cfg_name] = cr.results, cr.manifests
        n_facets = len(cr.facets)
        lake_steps = cr.manifests.get("lake", {})
        corpora_meta[cfg_name] = {"config": corpus["config"], "stats": corpus["stats"], "targets": corpus["targets"],
                                  "config_sha256": sha256_file(cfg_path), "scenarios": scens, "facets": list(cr.facets),
                                  "expected_files": {step: n_facets * len({k.split(':')[0] for k in m})
                                                     for step, m in lake_steps.items()},
                                  "modules_at_end": len(cr.modules.get("lake", []))}
        # the reference must be complete: every module it built has every facet
        for step, m in lake_steps.items():
            mods_needed = len(cr.modules["lake"]) if not step.startswith(("cold",)) else corpus["stats"]["modules"]
            if len(m) < n_facets * min(mods_needed, len({k.split(':')[0] for k in m})):
                log(f"warning: lake's artifacts at {cfg_name}/{step} are incomplete")
        if not args.keep:
            cr.cleanup()
    for alias, t in tools.items():
        if "alias" in t:
            for cfg_name in measurements:
                measurements[cfg_name][alias] = measurements[cfg_name][t["alias"]]
                manifests_all[cfg_name][alias] = manifests_all[cfg_name][t["alias"]]
    correctness = {}
    if "lake" in tool_names:
        for tool_name in tool_names:
            if tool_name != "lake":
                correctness[tool_name] = correctness_for(tool_name, manifests_all, measurements, corpora_meta)

    result = {
        "schema": SCHEMA_VERSION,
        "timestamp": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "label": args.label,
        "suite": suite_name,
        "sequential": suite["sequential"],
        "standard": standard,
        "nonstandard_reasons": nonstandard,
        "targets": targets,
        "argv": sys.argv[1:],
        "duration_s": round(time.time() - t_start, 1),
        "machine": machine,
        "toolchain": toolchain,
        "harness": {"run_py_sha256": sha256_file(Path(__file__)), "generate_py_sha256": sha256_file(GEN),
                    "edits_py_sha256": sha256_file(HERE / "edits.py"), "scrubbed_env": scrubbed,
                    "build_timeout_s": BUILD_TIMEOUT_S},
        "git": {"commit": head_commit, "branch": head_branch, "dirty": dirty, "dirty_ignores": "research/"},
        "tools": {k: {kk: vv for kk, vv in v.items() if kk != "env"} for k, v in tools.items()},
        "tool_order": tool_names,
        "tool_order_per_corpus": orders,
        "repeat": repeat,
        "repeat_fast": repeat_fast,
        "noise_rule": NOISE_RULE,
        "scenarios": scenarios,
        "corpora": corpora_meta,
        "measurements": measurements,
        "speedup": ratios(measurements, tool_names),
        "correctness": correctness,
    }
    result["verdict"] = verdict_for_result(result)
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    fname = f"{stamp}-{machine['id']}-{result['git']['commit'][:7]}.json"
    out = Path(args.out) if args.out else (RESULTS / fname if standard else WORK / "results" / fname)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(result, indent=1) + "\n", encoding="utf-8")
    print_report(result)
    log(f"wrote {out}")
    if result["verdict"]["overall"] == "failure" or ("candidate" in correctness and not correct(correctness["candidate"])):
        sys.exit(2)


if __name__ == "__main__":
    main()
