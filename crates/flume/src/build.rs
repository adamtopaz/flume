//! Building a package's Lean modules.
//!
//! 1. **Discovery.** The targets' modules (from library globs) and, transitively, every
//!    local module they import. An import that is not local must be a module of the
//!    toolchain (`Init`, `Std`, `Lean`, `Lake`, ...), which needs no building. A source
//!    whose file status is unchanged since the last build is neither read nor parsed.
//! 2. **Keys.** A module's key hashes everything its outputs depend on: the toolchain's
//!    githash, the package name, the library's Lean options and arguments, the module's
//!    name and source, and for each local import the content of that import's artifacts
//!    and of what it re-exports, combined exactly as Lake 5 does (`addImport`,
//!    `computeExportInfo` in `Lake/Build/Module.lean`): a module-system importer depends
//!    on an imported module's `.olean` and, through its exported imports, theirs; `meta
//!    import` adds the IR, `import all` everything; a non-module importer depends on all
//!    artifacts of everything below it. The hashes are Merkle-style, linear in the graph.
//!    An unchanged key with intact outputs means up to date, and a rebuild that
//!    reproduces an import's artifacts leaves its dependents alone, as in Lake.
//! 3. **Compilation.** Exactly what Lake 5 runs: `lean <args> <file> -o <olean> -i <ilean>
//!    -c <c> --setup <setup.json> --json` with `LEAN_PATH` set to the package's library
//!    directory, the setup file listing the import artifacts Lake would list (its
//!    `fetchTransImportArts` walk). Outputs land in Lake's build layout.
//! 4. **Scheduling.** A pool of `jobs` workers takes ready modules, tallest first
//!    (longest chain of dependents), so the critical path starts as early as possible.

use crate::config::{Lib, Package};
use crate::header;
use crate::store::{Built, FileStat, SourceInfo, State, StoredImport};
use crate::toolchain::Toolchain;
use serde_json::{json, Map, Value as Json};
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

const KEY_VERSION: &str = "flume-key-1";
const MODULE_FACETS: [&str; 7] = ["olean", "olean.server", "olean.private", "ilean", "ir", "ir.sig", "c"];
const LEGACY_FACETS: [&str; 3] = ["olean", "ilean", "c"];

pub struct Options {
    pub targets: Vec<String>,
    pub jobs: usize,
    pub verbose: bool,
}

/// An import of a local module (toolchain modules need no building and are covered
/// by the toolchain's githash).
#[derive(Clone, Copy)]
struct LocalImport {
    idx: usize,
    all: bool,
    exported: bool,
    meta: bool,
}

struct Module {
    name: String,
    comps: Vec<String>,
    file: PathBuf,
    rel_file: String,
    lib: usize,
    is_module: bool,
    src_hash: String,
    imports: Vec<LocalImport>,
    deps: Vec<usize>,
    dependents: Vec<usize>,
    height: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Pending,
    UpToDate,
    Built,
    Failed,
    Blocked,
}

/// What dependents need of a finished module: Lake's artifact and transitive traces.
#[derive(Clone, Default)]
struct Done {
    arts: Hash,
    meta_arts: Hash,
    all_arts: Hash,
    trans: Hash,
    meta_trans: Hash,
    all_trans: Hash,
    legacy_trans: Hash,
}

type Hash = [u8; 32];

fn mix(parts: &[&[u8]]) -> Hash {
    let mut h = blake3::Hasher::new();
    for p in parts {
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p);
    }
    *h.finalize().as_bytes()
}

pub fn facets(is_module: bool) -> &'static [&'static str] {
    if is_module { &MODULE_FACETS } else { &LEGACY_FACETS }
}

pub fn artifact_path(pkg: &Package, comps: &[String], facet: &str) -> PathBuf {
    let rel = comps.join("/");
    if facet == "c" {
        pkg.ir_dir.join(format!("{rel}.c"))
    } else {
        pkg.lib_dir.join(format!("{rel}.{facet}"))
    }
}

fn hash_file(p: &Path) -> std::io::Result<String> {
    Ok(blake3::hash(&std::fs::read(p)?).to_hex().to_string())
}

fn module_file(lib: &Lib, comps: &[String]) -> PathBuf {
    let mut p = lib.src_dir.clone();
    for c in &comps[..comps.len() - 1] {
        p.push(c);
    }
    p.push(format!("{}.lean", comps[comps.len() - 1]));
    p
}

/// The modules a library's globs select (files under its source directory).
fn lib_modules(lib: &Lib) -> Result<Vec<Vec<String>>, String> {
    use crate::config::Glob;
    let mut out = Vec::new();
    for g in &lib.globs {
        let (base, me, subs) = match g {
            Glob::One(n) => (n, true, false),
            Glob::Submodules(n) => (n, false, true),
            Glob::AndSubmodules(n) => (n, true, true),
        };
        if me && module_file(lib, base).is_file() {
            out.push(base.clone());
        }
        if subs {
            let mut dir = lib.src_dir.clone();
            for c in base {
                dir.push(c);
            }
            let mut found = Vec::new();
            walk(&dir, base, &mut found)?;
            found.sort();
            out.extend(found);
        }
    }
    Ok(out)
}

fn walk(dir: &Path, prefix: &[String], out: &mut Vec<Vec<String>>) -> Result<(), String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok(()) };
    for e in rd {
        let e = e.map_err(|e| e.to_string())?;
        let name = e.file_name().to_string_lossy().to_string();
        let ft = e.file_type().map_err(|e| e.to_string())?;
        if ft.is_dir() {
            let mut p = prefix.to_vec();
            p.push(name);
            walk(&e.path(), &p, out)?;
        } else if let Some(stem) = name.strip_suffix(".lean") {
            let mut p = prefix.to_vec();
            p.push(stem.to_string());
            out.push(p);
        }
    }
    Ok(())
}

struct Graph {
    mods: Vec<Module>,
}

fn discover(pkg: &Package, tc: &Toolchain, state: &State, targets: &[String],
            sources: &mut HashMap<String, SourceInfo>) -> Result<Graph, String> {
    let mut roots: Vec<(Vec<String>, Option<usize>)> = Vec::new();
    let names: Vec<String> = if !targets.is_empty() {
        targets.to_vec()
    } else if !pkg.default_targets.is_empty() {
        pkg.default_targets.clone()
    } else {
        pkg.libs.iter().map(|l| l.name.clone()).collect()
    };
    for t in &names {
        if let Some(li) = pkg.libs.iter().position(|l| &l.name == t) {
            for m in lib_modules(&pkg.libs[li])? {
                roots.push((m, Some(li)));
            }
        } else {
            roots.push((crate::config::split_name(t), None));
        }
    }
    let mut index: HashMap<Vec<String>, usize> = HashMap::new();
    let mut mods: Vec<Module> = Vec::new();
    let mut imports_of: Vec<Vec<StoredImport>> = Vec::new();
    let mut queue: VecDeque<(Vec<String>, Option<usize>, Option<String>)> =
        roots.into_iter().map(|(m, l)| (m, l, None)).collect();
    let toolchain_lib = tc.sysroot.join("lib").join("lean");
    while let Some((comps, lib_hint, importer)) = queue.pop_front() {
        if index.contains_key(&comps) {
            continue;
        }
        let lib = lib_hint.or_else(|| {
            pkg.libs.iter().position(|l| l.is_local_module(&comps) && module_file(l, &comps).is_file())
        });
        let Some(li) = lib else {
            let mut olean = toolchain_lib.clone();
            for c in &comps {
                olean.push(c);
            }
            if olean.with_extension("olean").is_file() {
                continue; // a toolchain module
            }
            let name = header::name_to_string(&comps);
            return Err(match importer {
                Some(by) => format!("unknown module `{name}` (imported by `{by}`)"),
                None => format!("unknown target `{name}`"),
            });
        };
        let file = module_file(&pkg.libs[li], &comps);
        let name = header::name_to_string(&comps);
        let stat = FileStat::of(&file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let info = match state.sources.get(&name) {
            Some(s) if s.stat == stat => s.clone(),
            _ => {
                let bytes = std::fs::read(&file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
                let text = String::from_utf8_lossy(&bytes);
                let h = header::parse(&text).map_err(|e| {
                    let line = text[..e.pos.min(text.len())].matches('\n').count() + 1;
                    format!("{}:{}: {}", file.display(), line, e.msg)
                })?;
                SourceInfo {
                    stat,
                    hash: blake3::hash(&bytes).to_hex().to_string(),
                    is_module: h.is_module,
                    imports: h
                        .imports
                        .into_iter()
                        .map(|i| StoredImport { module: i.module, all: i.all, exported: i.exported, meta: i.meta })
                        .collect(),
                }
            }
        };
        let rel_file = file.strip_prefix(&pkg.root).unwrap_or(&file).to_string_lossy().to_string();
        let imps: Vec<StoredImport> = info.imports.clone();
        for imp in &imps {
            if !index.contains_key(&imp.module) {
                queue.push_back((imp.module.clone(), None, Some(name.clone())));
            }
        }
        index.insert(comps.clone(), mods.len());
        mods.push(Module {
            name: name.clone(),
            comps,
            file,
            rel_file,
            lib: li,
            is_module: info.is_module,
            src_hash: info.hash.clone(),
            imports: Vec::new(),
            deps: Vec::new(),
            dependents: Vec::new(),
            height: 0,
        });
        imports_of.push(imps);
        sources.insert(name, info);
    }
    for (i, imps) in imports_of.iter().enumerate() {
        mods[i].imports = imps
            .iter()
            .filter_map(|imp| {
                index.get(&imp.module).map(|&idx| LocalImport { idx, all: imp.all, exported: imp.exported, meta: imp.meta })
            })
            .collect();
        let mut deps: Vec<usize> = mods[i].imports.iter().map(|li| li.idx).collect();
        deps.sort_unstable();
        deps.dedup();
        for &d in &deps {
            mods[d].dependents.push(i);
        }
        mods[i].deps = deps;
    }
    // topological order (detects cycles), then heights
    let n = mods.len();
    let mut indeg: Vec<usize> = mods.iter().map(|m| m.deps.len()).collect();
    let mut order: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut k = 0;
    while k < order.len() {
        let i = order[k];
        k += 1;
        for &j in &mods[i].dependents.clone() {
            indeg[j] -= 1;
            if indeg[j] == 0 {
                order.push(j);
            }
        }
    }
    if order.len() != n {
        let stuck: Vec<&str> = (0..n).filter(|&i| indeg[i] > 0).take(5).map(|i| mods[i].name.as_str()).collect();
        return Err(format!("import cycle among: {}", stuck.join(", ")));
    }
    for &i in order.iter().rev() {
        let h = mods[i].dependents.iter().map(|&j| mods[j].height + 1).max().unwrap_or(0);
        mods[i].height = h;
    }
    Ok(Graph { mods })
}

/// Lake's `arts` / `allArts` of a module, as paths.
fn import_arts(pkg: &Package, m: &Module, all: bool) -> Vec<Vec<String>> {
    let p = |f: &str| artifact_path(pkg, &m.comps, f).to_string_lossy().to_string();
    if !m.is_module {
        vec![vec![p("olean")]]
    } else if all {
        vec![vec![p("olean"), p("olean.server"), p("olean.private")], vec![p("ir.sig"), p("ir")]]
    } else {
        vec![vec![p("olean"), p("olean.server")], vec![p("ir.sig"), p("ir")]]
    }
}

/// The import artifacts Lake puts in a module's setup file (`fetchTransImportArts` and
/// `ModuleImportInfo.addImport` in Lake 5): the direct imports, then the modules they
/// make reachable (exported imports, widened by `import all` and `meta import`).
fn setup_import_arts(g: &Graph, m: &Module) -> Vec<(usize, bool)> {
    let non_module = !m.is_module;
    // (module, all?) in insertion order; Lean reads it as a map
    let mut s: Vec<(usize, bool)> = Vec::new();
    let mut pos: HashMap<usize, usize> = HashMap::new();
    for li in &m.imports {
        let all = non_module || li.all;
        match pos.get(&li.idx) {
            Some(&k) => {
                if all {
                    s[k].1 = true;
                }
            }
            None => {
                pos.insert(li.idx, s.len());
                s.push((li.idx, all));
            }
        }
    }
    struct Entry {
        idx: usize,
        all: bool,
        meta: bool,
    }
    let enqueue = |all: bool, meta: bool, d: usize, q: &mut Vec<Entry>| {
        for imp in g.mods[d].imports.iter().rev() {
            let reachable = all || imp.exported;
            let ia = non_module || (all && imp.all);
            let nm = meta || (reachable && imp.meta);
            if reachable || nm {
                q.push(Entry { idx: imp.idx, all: ia, meta: nm });
            }
        }
    };
    let mut q: Vec<Entry> = Vec::new();
    for li in m.imports.iter().rev() {
        enqueue(non_module || li.all, li.meta, li.idx, &mut q);
    }
    let mut all_visited: std::collections::HashSet<usize> = Default::default();
    let mut meta_visited: std::collections::HashSet<usize> = Default::default();
    while let Some(Entry { idx, all, meta }) = q.pop() {
        let do_all = all && !all_visited.contains(&idx);
        let do_meta = meta && !meta_visited.contains(&idx);
        let existing = pos.get(&idx).copied();
        if let Some(k) = existing {
            let widenable = g.mods[idx].is_module; // a module-system entry carries `.server`
            if !(widenable && (do_all || do_meta)) {
                continue;
            }
            let _ = k;
        }
        if all {
            all_visited.insert(idx);
        }
        if meta {
            meta_visited.insert(idx);
        }
        let had_all = existing.map_or(false, |k| s[k].1 && g.mods[idx].is_module);
        let want_all = all_visited.contains(&idx) || had_all;
        match existing {
            Some(k) => s[k].1 = want_all,
            None => {
                pos.insert(idx, s.len());
                s.push((idx, want_all));
            }
        }
        enqueue(all, meta, idx, &mut q);
    }
    s
}

struct Shared<'a> {
    pkg: &'a Package,
    tc: &'a Toolchain,
    g: &'a Graph,
    state: &'a State,
    verbose: bool,
    sched: Mutex<Sched>,
    cv: Condvar,
    out: Mutex<()>,
}

struct Sched {
    ready: BinaryHeap<(u32, std::cmp::Reverse<usize>)>,
    pending: Vec<usize>,
    status: Vec<Status>,
    done: Vec<Done>,
    finished: usize,
    compiled: usize,
    new_built: HashMap<String, Option<Built>>,
}

impl Shared<'_> {
    fn key(&self, i: usize, done: &[Done]) -> String {
        let m = &self.g.mods[i];
        let lib = &self.pkg.libs[m.lib];
        let mut h = blake3::Hasher::new();
        let mut put = |b: &[u8]| {
            h.update(&(b.len() as u64).to_le_bytes());
            h.update(b);
        };
        put(KEY_VERSION.as_bytes());
        put(self.tc.githash.as_bytes());
        put(self.pkg.name.as_bytes());
        put(serde_json::to_string(&lib.options).unwrap_or_default().as_bytes());
        put(lib.lean_args.join("\0").as_bytes());
        put(m.name.as_bytes());
        put(m.src_hash.as_bytes());
        put(if m.is_module { b"module" } else { b"legacy" });
        // Lake's `disambiguationHash` of each import
        for li in &m.imports {
            let d = &done[li.idx];
            let (t, a) = if !m.is_module {
                (&d.legacy_trans, &d.all_arts)
            } else if li.all {
                (&d.all_trans, &d.all_arts)
            } else if li.meta {
                (&d.meta_trans, &d.meta_arts)
            } else {
                (&d.trans, &d.arts)
            };
            put(self.g.mods[li.idx].name.as_bytes());
            put(t);
            put(a);
        }
        h.finalize().to_hex().to_string()
    }

    /// Lake's `ModuleExportInfo` traces of a finished module.
    fn export_info(&self, i: usize, outputs: &[(String, String)], done: &[Done]) -> Done {
        let m = &self.g.mods[i];
        let out = |f: &str| outputs.iter().find(|(k, _)| k == f).map(|(_, v)| v.as_bytes()).unwrap_or(b"");
        let (arts, meta_arts, all_arts) = if m.is_module {
            (
                mix(&[b"arts", out("olean")]),
                mix(&[b"meta", out("olean"), out("ir.sig"), out("ir")]),
                mix(&[b"all", out("olean"), out("olean.server"), out("olean.private"), out("ir.sig"), out("ir")]),
            )
        } else {
            (mix(&[b"arts", out("olean")]), mix(&[b"meta", out("olean")]), mix(&[b"all", out("olean")]))
        };
        let (mut trans, mut meta_trans, mut all_trans, mut legacy_trans) =
            (blake3::Hasher::new(), blake3::Hasher::new(), blake3::Hasher::new(), blake3::Hasher::new());
        for li in &m.imports {
            let d = &done[li.idx];
            legacy_trans.update(&d.legacy_trans);
            legacy_trans.update(&d.all_arts);
            let (t, a) = if li.all {
                (&d.all_trans, &d.all_arts)
            } else if li.meta {
                (&d.meta_trans, &d.meta_arts)
            } else {
                (&d.trans, &d.arts)
            };
            all_trans.update(t);
            all_trans.update(a);
            meta_trans.update(&d.meta_trans);
            meta_trans.update(&d.meta_arts);
            if li.exported {
                let (t, a) = if li.meta { (&d.meta_trans, &d.meta_arts) } else { (&d.trans, &d.arts) };
                trans.update(t);
                trans.update(a);
            }
        }
        Done {
            arts,
            meta_arts,
            all_arts,
            trans: *trans.finalize().as_bytes(),
            meta_trans: *meta_trans.finalize().as_bytes(),
            all_trans: *all_trans.finalize().as_bytes(),
            legacy_trans: *legacy_trans.finalize().as_bytes(),
        }
    }

    /// The remembered outputs if the module is up to date; also a refreshed record when
    /// the `.olean`'s file status changed but its content did not (e.g. a copied checkout).
    fn up_to_date(&self, i: usize, key: &str) -> Option<(Vec<(String, String)>, Option<Built>)> {
        let m = &self.g.mods[i];
        let b = self.state.built.get(&m.name)?;
        if b.key != key {
            return None;
        }
        let olean = artifact_path(self.pkg, &m.comps, "olean");
        let stat = FileStat::of(&olean).ok()?;
        if stat == b.olean_stat {
            return Some((b.outputs.clone(), None));
        }
        // status changed: trust the outputs only if they still hash to what was built
        for (f, h) in &b.outputs {
            if hash_file(&artifact_path(self.pkg, &m.comps, f)).ok()? != *h {
                return None;
            }
        }
        Some((b.outputs.clone(), Some(Built { key: key.to_string(), outputs: b.outputs.clone(), olean_stat: stat })))
    }

    fn setup_json(&self, i: usize) -> Json {
        let m = &self.g.mods[i];
        let lib = &self.pkg.libs[m.lib];
        let mut arts = Map::new();
        for (d, all) in setup_import_arts(self.g, m) {
            let dm = &self.g.mods[d];
            arts.insert(dm.name.clone(), json!(import_arts(self.pkg, dm, all)));
        }
        json!({
            "plugins": [],
            "package": self.pkg.name,
            "options": Json::Object(lib.options.clone()),
            "name": m.name,
            "isModule": m.is_module,
            "importArts": Json::Object(arts),
            "dynlibs": [],
        })
    }

    fn compile(&self, i: usize) -> Result<(Vec<(String, String)>, FileStat, Vec<String>, u128), (Vec<String>, String)> {
        let m = &self.g.mods[i];
        let lib = &self.pkg.libs[m.lib];
        let olean = artifact_path(self.pkg, &m.comps, "olean");
        let ilean = artifact_path(self.pkg, &m.comps, "ilean");
        let c = artifact_path(self.pkg, &m.comps, "c");
        let setup = c.with_extension("setup.json");
        for p in [&olean, &c] {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
        }
        let fail = |msgs: Vec<String>, e: String| Err((msgs, e));
        // Like Lake (`Module.clearOutputArtifacts`): remove the old outputs first, so a
        // failed compile never leaves stale artifacts behind.
        for f in MODULE_FACETS.iter().chain(["ltar", "bc"].iter()) {
            let _ = std::fs::remove_file(artifact_path(self.pkg, &m.comps, f));
        }
        if let Err(e) = std::fs::write(&setup, serde_json::to_vec_pretty(&self.setup_json(i)).unwrap_or_default()) {
            return fail(vec![], format!("cannot write {}: {e}", setup.display()));
        }
        let mut cmd = Command::new(&self.tc.lean);
        cmd.args(&lib.lean_args)
            .arg(&m.file)
            .arg("-o").arg(&olean)
            .arg("-i").arg(&ilean)
            .arg("-c").arg(&c)
            .arg("--setup").arg(&setup)
            .arg("--json")
            .env("LEAN_PATH", &self.pkg.lib_dir)
            .current_dir(&self.pkg.root)
            .stdin(Stdio::null());
        let t0 = Instant::now();
        let out = match cmd.output() {
            Ok(o) => o,
            Err(e) => return fail(vec![], format!("cannot run {}: {e}", self.tc.lean.display())),
        };
        let ms = t0.elapsed().as_millis();
        let mut msgs = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines().chain(String::from_utf8_lossy(&out.stderr).lines()) {
            if line.trim().is_empty() {
                continue;
            }
            msgs.push(render_message(line, &m.rel_file));
        }
        if !out.status.success() {
            return fail(msgs, String::new());
        }
        let mut outputs = Vec::new();
        for f in facets(m.is_module) {
            let p = artifact_path(self.pkg, &m.comps, f);
            match hash_file(&p) {
                Ok(h) => outputs.push((f.to_string(), h)),
                Err(e) => return fail(msgs, format!("missing output {}: {e}", p.display())),
            }
        }
        let stat = match FileStat::of(&olean) {
            Ok(s) => s,
            Err(e) => return fail(msgs, format!("cannot stat {}: {e}", olean.display())),
        };
        Ok((outputs, stat, msgs, ms))
    }

    fn print(&self, text: &str) {
        let _g = self.out.lock().unwrap();
        let mut so = std::io::stdout().lock();
        let _ = so.write_all(text.as_bytes());
        let _ = so.flush();
    }

    fn worker(&self) {
        let total = self.g.mods.len();
        loop {
            let i = {
                let mut s = self.sched.lock().unwrap();
                loop {
                    if let Some((_, std::cmp::Reverse(i))) = s.ready.pop() {
                        break i;
                    }
                    if s.finished == total {
                        return;
                    }
                    s = self.cv.wait(s).unwrap();
                }
            };
            // all dependencies of i are finished, so their `done` entries are final
            let key = {
                let s = self.sched.lock().unwrap();
                self.key(i, &s.done)
            };
            let m = &self.g.mods[i];
            let (status, outputs, built) = match self.up_to_date(i, &key) {
                Some((outputs, refreshed)) => (Status::UpToDate, outputs, refreshed.map(Some)),
                None => match self.compile(i) {
                    Ok((outputs, stat, msgs, ms)) => {
                        let n = {
                            let mut s = self.sched.lock().unwrap();
                            s.compiled += 1;
                            s.compiled
                        };
                        let warn = msgs.iter().any(|l| l.starts_with("warning:"));
                        let mut text = format!("{} [{n}/{total}] Built {} ({ms}ms)\n", if warn { "⚠" } else { "✔" }, m.name);
                        if warn || self.verbose {
                            for l in &msgs {
                                text.push_str(l);
                                text.push('\n');
                            }
                        }
                        self.print(&text);
                        let b = Built { key: key.clone(), outputs: outputs.clone(), olean_stat: stat };
                        (Status::Built, outputs, Some(Some(b)))
                    }
                    Err((msgs, err)) => {
                        let n = {
                            let mut s = self.sched.lock().unwrap();
                            s.compiled += 1;
                            s.compiled
                        };
                        let mut text = format!("✖ [{n}/{total}] Building {}\n", m.name);
                        for l in &msgs {
                            text.push_str(l);
                            text.push('\n');
                        }
                        if !err.is_empty() {
                            text.push_str(&format!("error: {err}\n"));
                        }
                        self.print(&text);
                        (Status::Failed, Vec::new(), Some(None))
                    }
                },
            };
            let mut s = self.sched.lock().unwrap();
            if status != Status::Failed {
                let info = self.export_info(i, &outputs, &s.done);
                s.done[i] = info;
            }
            s.status[i] = status;
            if let Some(b) = built {
                s.new_built.insert(m.name.clone(), b);
            }
            s.finished += 1;
            let mut stack = vec![i];
            while let Some(j) = stack.pop() {
                let bad = matches!(s.status[j], Status::Failed | Status::Blocked);
                for &d in &self.g.mods[j].dependents {
                    s.pending[d] -= 1;
                    if s.pending[d] == 0 {
                        let blocked = bad || self.g.mods[d].deps.iter().any(|&x| matches!(s.status[x], Status::Failed | Status::Blocked));
                        if blocked {
                            // keep what was remembered about it: its inputs are unknown now
                            s.status[d] = Status::Blocked;
                            s.finished += 1;
                            stack.push(d);
                        } else {
                            let h = self.g.mods[d].height;
                            s.ready.push((h, std::cmp::Reverse(d)));
                        }
                    }
                }
            }
            drop(s);
            self.cv.notify_all();
        }
    }
}

fn render_message(line: &str, rel_file: &str) -> String {
    match serde_json::from_str::<Json>(line) {
        Ok(Json::Object(o)) => {
            let sev = o.get("severity").and_then(Json::as_str).unwrap_or("info");
            let sev = if sev == "information" { "info" } else { sev };
            let data = o.get("data").and_then(Json::as_str).unwrap_or("").trim_end();
            match o.get("pos") {
                Some(p) => format!(
                    "{sev}: {rel_file}:{}:{}: {data}",
                    p.get("line").and_then(Json::as_u64).unwrap_or(0),
                    p.get("column").and_then(Json::as_u64).unwrap_or(0)
                ),
                None => format!("{sev}: {rel_file}: {data}"),
            }
        }
        _ => line.to_string(),
    }
}

/// Build the targets; returns whether everything succeeded.
pub fn build(pkg: &Package, tc: &Toolchain, opts: &Options) -> Result<bool, String> {
    let state_path = pkg.root.join(".lake").join("flume").join("state.json");
    let state = State::load(&state_path, &tc.githash);
    let mut sources = HashMap::new();
    let g = discover(pkg, tc, &state, &opts.targets, &mut sources)?;
    let n = g.mods.len();
    let shared = Shared {
        pkg,
        tc,
        g: &g,
        state: &state,
        verbose: opts.verbose,
        sched: Mutex::new(Sched {
            ready: BinaryHeap::new(),
            pending: g.mods.iter().map(|m| m.deps.len()).collect(),
            status: vec![Status::Pending; n],
            done: vec![Done::default(); n],
            finished: 0,
            compiled: 0,
            new_built: HashMap::new(),
        }),
        cv: Condvar::new(),
        out: Mutex::new(()),
    };
    {
        let mut s = shared.sched.lock().unwrap();
        for (i, m) in g.mods.iter().enumerate() {
            if m.deps.is_empty() {
                s.ready.push((m.height, std::cmp::Reverse(i)));
            }
        }
    }
    let jobs = opts.jobs.max(1).min(n.max(1));
    std::thread::scope(|sc| {
        for _ in 0..jobs {
            sc.spawn(|| shared.worker());
        }
    });
    let s = shared.sched.into_inner().unwrap();
    let failed: Vec<&str> = (0..n).filter(|&i| s.status[i] == Status::Failed).map(|i| g.mods[i].name.as_str()).collect();
    // persist what changed
    let sources_changed = sources.iter().any(|(k, v)| state.sources.get(k).map_or(true, |o| o.stat != v.stat));
    if sources_changed || !s.new_built.is_empty() {
        let mut new_state = state.clone();
        new_state.sources.extend(sources);
        for (k, v) in s.new_built {
            match v {
                Some(b) => {
                    new_state.built.insert(k, b);
                }
                None => {
                    new_state.built.remove(&k);
                }
            }
        }
        new_state.save(&state_path).map_err(|e| format!("cannot write {}: {e}", state_path.display()))?;
    }
    if failed.is_empty() {
        println!("Build completed successfully ({} jobs).", s.compiled);
        Ok(true)
    } else {
        println!("Some required targets logged failures:");
        for f in &failed {
            println!("- {f}");
        }
        println!("error: build failed");
        Ok(false)
    }
}
