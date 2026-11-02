"""Source edits the benchmark harness applies to generated corpora.

Every edit is deterministic in (module, kind, stamp), keeps the corpus building,
and is applied identically to every tool's copy, so all tools see the same
sequence of sources. Stamps are unique per edit, so no edit ever reproduces an
earlier source text.

Edit kinds (`KINDS`), by what they change:

  proof      the proof of the module's `val_ge`; its statement stays the same
  private    adds a private theorem
  public     adds a public theorem (the module's interface grows)
  statement  changes the statement of `val_ge`, which every direct dependent
             uses (weakened by `+ 0 * stamp`, so dependents still build)
  body       changes the body of the exposed `val` in a definitionally equal
             way (`C` becomes `(C + 0 * stamp)`); dependents still build, but
             the proof of their `imp_body` theorem, and so their artifacts,
             change

In module-system corpora (`module_system: true`) `proof` and `private` leave
the module's interface unchanged (stock Lake rebuilds only the module), while
`public`, `statement`, and `body` change it (stock Lake rebuilds every
dependent). In non-module corpora every edit changes the module's `.olean`.

Breaking edits (`apply_break`) must make every direct dependent fail:
  rename     renames `val_ge`, which every direct dependent's proof uses
  body       changes the constant in `val`, which every dependent restates by
             `rfl` (a change of definition body only)

`new_module` writes a new module importing existing ones; the corpus's
`Corpus.+` glob picks it up.
"""

from __future__ import annotations

import re
from pathlib import Path

KINDS = ("proof", "private", "public", "statement", "body")
INTERFACE_KINDS = ("public", "statement", "body")
PRIVATE_KINDS = ("proof", "private")

_PROOF = re.compile(r"(theorem val_ge \(n : Nat\) : [^\n]*:= by\n)(  have _bench_proof[^\n]*\n)?")
_STATEMENT = re.compile(r"(theorem val_ge \(n : Nat\) : n ≤ val n)( \+ 0 \* \d+)?( := by)")
_BODY = re.compile(r"(def val \(n : Nat\) : Nat :=\s+.*? \+ )(\d+|\((\d+) \+ 0 \* \d+\))\n", re.S)


def module_file(corpus_dir: Path, corpus: dict, name: str) -> Path:
    for m in corpus["modules"]:
        if m["name"] == name:
            return corpus_dir / m["file"]
    extra = corpus_dir / (name.replace(".", "/") + ".lean")
    if extra.exists():
        return extra
    raise KeyError(name)


def _sub_once(pattern: re.Pattern, repl, text: str, what: str, name: str) -> str:
    new, n = pattern.subn(repl, text, count=1)
    if n != 1:
        raise RuntimeError(f"edit {what!r} does not apply to {name}")
    return new


def edit_text(text: str, name: str, kind: str, stamp: int, module_system: bool) -> str:
    """The module's source after edit `kind` with `stamp`."""
    if kind == "proof":
        return _sub_once(_PROOF, lambda m: m.group(1) + f"  have _bench_proof : ({stamp} : Nat) = {stamp} := rfl\n",
                         text, kind, name)
    if kind == "private":
        return text + (f"\nprivate theorem {name}.bench_private_{stamp} (n : Nat) : n ≤ n + {stamp} := by\n"
                       f"  omega\n")
    if kind == "public":
        vis = "public " if module_system else ""
        return text + (f"\n{vis}theorem {name}.bench_edit_{stamp} (n : Nat) : {name}.val n ≤ {name}.val n + {stamp} := by\n"
                       f"  omega\n")
    if kind == "statement":
        return _sub_once(_STATEMENT, lambda m: f"{m.group(1)} + 0 * {stamp}{m.group(3)}", text, kind, name)
    if kind == "body":
        def repl(m: re.Match) -> str:
            const = m.group(3) or m.group(2)
            return f"{m.group(1)}({const} + 0 * {stamp})\n"
        return _sub_once(_BODY, repl, text, kind, name)
    raise ValueError(f"unknown edit kind {kind!r}")


def apply_edit(corpus_dir: Path, corpus: dict, name: str, kind: str, stamp: int) -> None:
    f = module_file(corpus_dir, corpus, name)
    text = f.read_text(encoding="utf-8")
    f.write_text(edit_text(text, name, kind, stamp, corpus["config"].get("module_system", False)), encoding="utf-8")


def apply_break(corpus_dir: Path, corpus: dict, name: str, kind: str) -> str:
    """Make a change to `name` that every direct dependent must notice. Returns the original text."""
    f = module_file(corpus_dir, corpus, name)
    original = f.read_text(encoding="utf-8")
    if kind == "rename":
        broken = original.replace("theorem val_ge ", "theorem val_ge_renamed ", 1)
    elif kind == "body":
        m = _BODY.search(original)
        if m:
            const = int(m.group(3) or m.group(2))
            broken = original[: m.start(2)] + str(const + 1) + original[m.end(2):]
        else:
            broken = original
    else:
        raise ValueError(kind)
    if broken == original:
        raise RuntimeError(f"could not apply breaking edit {kind!r} to {name}")
    f.write_text(broken, encoding="utf-8")
    return original


def new_module(corpus_dir: Path, corpus: dict, stamp: int, imports: list[str]) -> str:
    """Write a new module `Corpus.X<stamp>` importing `imports`; return its name."""
    name = f"Corpus.X{stamp:05d}"
    mod = corpus["config"].get("module_system", False)
    lines = []
    if mod:
        lines += ["module", ""]
    lines += [f"{'public import' if mod else 'import'} {i}" for i in imports]
    lines += [""]
    if mod:
        lines += ["public section", ""]
    terms = " + ".join(f"{imp}.val (n + {k})" for k, imp in enumerate(imports))
    haves = "\n".join(f"  have h{k} := {imp}.val_ge (n + {k})" for k, imp in enumerate(imports))
    lines += [f"namespace {name}", "",
              f"def val (n : Nat) : Nat := {terms} + {stamp % 7 + 1}", "",
              f"theorem val_ge (n : Nat) : n ≤ val n := by\n  unfold val\n{haves}\n  omega", "",
              f"end {name}"]
    if mod:
        lines += ["", "end"]
    lines += [""]
    path = corpus_dir / (name.replace(".", "/") + ".lean")
    path.write_text("\n".join(lines), encoding="utf-8")
    return name
