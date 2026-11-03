#!/usr/bin/env python3
"""Check flume's `lakefile.lean` support against stock Lake.

Usage: python3 tests/check_lakefile_lean.py [CONFIG.json...]
For each benchmark corpus config (default: bench/configs/small-*.json) the corpus is
generated, its `lakefile.toml` is rewritten as `lakefile.lean` (`lake translate-config
lean`), and the package is built with stock Lake and with flume: the artifacts must be
identical. A hand-written configuration with two libraries, package and library options,
a helper definition, and `meta if` is checked the same way. Then: the translation is
cached (a no-op build does not run Lake), an edit to `lakefile.lean` is picked up,
`lakefile.lean` wins over a `lakefile.toml` next to it (as in Lake), and declarations that
Lake's translation would drop (`script`, `target`) or that flume does not support
(`require`) are refused. Needs `lake` on PATH (elan) and a built flume
(`cargo build --release`). Exit status 1 on any failure.
"""
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FLUME = ROOT / "target" / "release" / "flume"
FACETS = (".olean", ".olean.server", ".olean.private", ".ilean", ".ir", ".ir.sig", ".c")

CUSTOM = {
    "lakefile.lean": """import Lake
open Lake DSL

def heartbeats : Nat := 100000 * 3

package custom where
  leanOptions := #[⟨`autoImplicit, false⟩]
  srcDir := "src"

@[default_target]
lean_lib A where
  roots := #[`A]
  moreLeanArgs := #["-DmaxRecDepth=600"]
  weakLeanArgs := #[s!"-DmaxHeartbeats={heartbeats}"]

meta if (get_config? noB).isNone then
@[default_target]
lean_lib B where
  globs := #[.andSubmodules `B]
  leanOptions := #[⟨`pp.all, true⟩]
""",
    "src/A.lean": "import A.Sub\n\ndef a : Nat := sub + 1\n",
    "src/A/Sub.lean": "def sub : Nat := 41\n\ntheorem sub_pos : 0 < sub := by decide\n",
    "src/B.lean": "import A\n\ndef b : Nat := a * 2\n",
    "src/B/C.lean": "import B\n\ntheorem c : b = 84 := by decide\n",
}

failures: list[str] = []


def check(cond: bool, what: str, detail: str = "") -> None:
    print(("ok   " if cond else "FAIL ") + what + ("" if cond or not detail else "\n" + detail))
    if not cond:
        failures.append(what)


def env_without_tools() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith(("FLUME_", "LEAN_", "LAKE_"))}


def run(cmd: list[str], cwd: Path, env: dict) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True)


def artifacts(d: Path) -> dict[str, str]:
    b = d / ".lake" / "build"
    return {str(p.relative_to(b)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(b.rglob("*")) if p.is_file() and p.name.endswith(FACETS)}


def compiled(out: str) -> int:
    return sum(1 for line in out.splitlines() if line.startswith(("✔", "⚠")) and "] Built " in line)


def both(src: Path, base: Path, name: str, env: dict) -> tuple[Path, Path]:
    """Build copies of `src` with Lake and with flume; return the two checkouts."""
    lake_dir, flume_dir = base / f"{name}-lake", base / f"{name}-flume"
    shutil.copytree(src, lake_dir)
    shutil.copytree(src, flume_dir)
    pl = run(["lake", "build"], lake_dir, env)
    pf = run([str(FLUME), "build"], flume_dir, env)
    check(pl.returncode == 0, f"{name}: lake builds it", pl.stdout[-2000:] + pl.stderr[-2000:])
    check(pf.returncode == 0, f"{name}: flume builds it", pf.stdout[-2000:] + pf.stderr[-2000:])
    a, b = artifacts(lake_dir), artifacts(flume_dir)
    check(bool(a) and a == b, f"{name}: identical artifacts ({len(b)} files vs {len(a)})")
    return lake_dir, flume_dir


def main() -> int:
    if not FLUME.exists() or shutil.which("lake") is None:
        print("needs a built flume (cargo build --release) and `lake` on PATH")
        return 1
    configs = [Path(a) for a in sys.argv[1:]] or sorted((ROOT / "bench" / "configs").glob("small-*.json"))
    with tempfile.TemporaryDirectory(prefix="flume-lakefile-lean-") as tmp:
        base = Path(tmp)
        env = env_without_tools()
        env["FLUME_CACHE_DIR"] = str(base / "flume-cache")

        for cfg in configs:
            src = base / cfg.stem
            subprocess.run([sys.executable, str(ROOT / "bench" / "gen" / "generate.py"), "--config", str(cfg),
                            "--out", str(src)], check=True, capture_output=True)
            p = run(["lake", "translate-config", "lean", "lakefile.lean"], src, env)
            check(p.returncode == 0, f"{cfg.stem}: rewritten as lakefile.lean", p.stderr)
            (src / "lakefile.toml").unlink()
            shutil.rmtree(src / ".lake", ignore_errors=True)
            both(src, base, cfg.stem, env)

        src = base / "custom"
        for rel, text in CUSTOM.items():
            (src / rel).parent.mkdir(parents=True, exist_ok=True)
            (src / rel).write_text(text, encoding="utf-8")
        shutil.copy(ROOT / "lean-toolchain", src / "lean-toolchain")
        lake_dir, d = both(src, base, "custom", env)

        # the translation is cached: a no-op build does not run Lake
        cached = list((d / ".lake" / "flume" / "config").glob("*.toml"))
        check(len(cached) == 1, f"the translation is cached ({[p.name for p in cached]})")
        stamp = cached[0].stat().st_mtime_ns if cached else 0
        t0 = time.monotonic()
        p = run([str(FLUME), "build"], d, env)
        dt = time.monotonic() - t0
        check(p.returncode == 0 and compiled(p.stdout) == 0 and dt < 0.5,
              f"a no-op build reuses it ({dt * 1000:.0f} ms)", p.stdout + p.stderr)
        check(bool(cached) and cached[0].exists() and cached[0].stat().st_mtime_ns == stamp,
              "and leaves it untouched")

        # an edit to the configuration is picked up (new arguments rebuild every module of A)
        for x in (d, lake_dir):
            lf = x / "lakefile.lean"
            lf.write_text(lf.read_text(encoding="utf-8").replace("maxRecDepth=600", "maxRecDepth=700"),
                          encoding="utf-8")
        p = run([str(FLUME), "build"], d, env)
        check(p.returncode == 0 and compiled(p.stdout) >= 2, f"an edited lakefile.lean is re-read ({compiled(p.stdout)} compiled)",
              p.stdout + p.stderr)
        run(["lake", "build"], lake_dir, env)
        check(artifacts(d) == artifacts(lake_dir), "and the rebuild matches Lake's")

        # lakefile.lean wins over lakefile.toml, as in Lake
        (d / "lakefile.toml").write_text('name = "other"\n\n[[lean_lib]]\nname = "Other"\n', encoding="utf-8")
        p = run([str(FLUME), "build"], d, env)
        check(p.returncode == 0 and compiled(p.stdout) == 0 and "Other" not in p.stdout,
              "lakefile.lean wins over lakefile.toml", p.stdout + p.stderr)
        (d / "lakefile.toml").unlink()

        # refusals
        good = (d / "lakefile.lean").read_text(encoding="utf-8")
        for extra, word in (("\nscript hello do\n  return 0\n", "script"),
                            ("\ntarget foo pkg : Unit := pure (Job.pure ())\n", "target"),
                            ('\nrequire other from "../other"\n', "require")):
            (d / "lakefile.lean").write_text(good + extra, encoding="utf-8")
            p = run([str(FLUME), "build"], d, env)
            check(p.returncode != 0 and word in (p.stdout + p.stderr),
                  f"a configuration with `{word}` is refused", p.stdout + p.stderr)
        (d / "lakefile.lean").write_text(good, encoding="utf-8")
    print("lakefile.lean check: " + ("ok" if not failures else f"{len(failures)} failure(s)"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
