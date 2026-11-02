/-
Copyright (c) 2026 Adam Topaz. All rights reserved.
Released under Apache 2.0 license as described in the file LICENSE.
-/
import Lean

/-!
# flume-worker

Commands:
* `flume-worker headers FILE...`: Lean's own parse (`Lean.parseImports'`, the parser
  Lake uses) of each file's header, one JSON object per line:
  `{"file", "isModule", "imports": [{"module", "importAll", "isExported", "isMeta"}]}`
  or `{"file", "error"}`.
* `flume-worker --version`
-/

open Lean

def printHeaders (files : List String) : IO UInt32 := do
  for f in files do
    let line ← try
        let h ← Lean.parseImports' (← IO.FS.readFile f) f
        pure <| Json.mkObj [("file", f), ("isModule", h.isModule), ("imports", toJson h.imports)]
      catch e =>
        pure <| Json.mkObj [("file", f), ("error", e.toString)]
    IO.println line.compress
  return 0

def main (args : List String) : IO UInt32 := do
  match args with
  | "headers" :: files => printHeaders files
  | ["--version"] =>
    IO.println s!"flume-worker {Lean.versionString} ({Lean.githash})"
    return 0
  | _ =>
    IO.eprintln "usage: flume-worker headers FILE... | --version"
    return 1
