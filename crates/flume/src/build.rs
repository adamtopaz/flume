//! Building a package's Lean modules.
//!
//! 1. **Discovery.** The targets' modules (from library globs, expanded as Lake does) and,
//!    transitively, every module they import that a library of the package can build
//!    (Lake's `findModule?`: the last library whose globs or roots cover it). Other imports
//!    are left to `lean`: toolchain modules (`Init`, `Std`, `Lean`, ...) need no building,
//!    and an unknown one makes the compile fail, as in Lake. A source whose file status is
//!    unchanged since the last build is neither read nor parsed. A module whose header
//!    cannot be read or parsed, or that is part of an import cycle, fails on its own and
//!    blocks only its dependents.
//! 2. **Keys.** A module's key hashes everything Lake traces: the toolchain's githash, the
//!    package name, the library's Lean options and `moreLeanArgs` (not `weakLeanArgs`),
//!    the module's name and source (line endings normalized, as `Hash.ofText` does), and
//!    for each local import the content of that import's artifacts and of what it
//!    re-exports, combined exactly as Lake 5 does (`addImport`, `computeExportInfo` in
//!    `Lake/Build/Module.lean`): a module-system importer depends on an imported module's
//!    `.olean` and, through its exported imports, theirs; `meta import` adds the IR,
//!    `import all` everything; a non-module importer depends on all artifacts below it.
//!    The hashes are Merkle-style, linear in the graph. An unchanged key with every
//!    output intact means up to date, and a rebuild that reproduces an import's
//!    artifacts leaves its dependents alone, as in Lake.
//! 3. **Compilation.** Exactly what Lake 5 runs: `lean <weak args> <more args> <file> -o
//!    <olean> -i <ilean> -c <c> --setup <setup.json> --json` with `LEAN_PATH` set to the
//!    package's library directory, after removing the module's old outputs; the setup
//!    file lists the import artifacts Lake would list (its `fetchTransImportArts` walk).
//!    A compile fails if `lean` exits non-zero or logs an error.
//! 4. **Scheduling.** A pool of `jobs` workers takes ready modules, tallest first
//!    (longest chain of dependents), so the critical path starts as early as possible.

use crate::config::{Glob, Lib, Package};
use crate::header;
use crate::store::{BuildLock, Built, FileStat, SourceInfo, State, StoredImport};
use crate::toolchain::Toolchain;
use serde_json::{json, Map, Value as Json};
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

const KEY_VERSION: &str = "flume-key-2";
const MODULE_FACETS: [&str; 7] = [
    "olean",
    "olean.server",
    "olean.private",
    "ilean",
    "ir",
    "ir.sig",
    "c",
];
const LEGACY_FACETS: [&str; 3] = ["olean", "ilean", "c"];

pub struct Options {
    pub targets: Vec<String>,
    pub jobs: usize,
    pub verbose: bool,
}

/// An import of a local module (toolchain modules need no building and are covered by
/// the toolchain's githash).
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
    /// why the module fails without being compiled (unreadable, unparsable, cycle)
    error: Option<String>,
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

/// Modules to start discovery from, each with the library that builds it.
type Roots = Vec<(Vec<String>, usize)>;

/// facet -> content hash of a module's outputs
type Outputs = Vec<(String, String)>;

/// The outcome of processing one module: status, outputs, and the state record to keep
/// (`Some(Some(b))` replace, `Some(None)` forget, `None` leave unchanged).
type Outcome = (Status, Outputs, Option<Option<Built>>);

fn mix(parts: &[&[u8]]) -> Hash {
    let mut h = blake3::Hasher::new();
    for p in parts {
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p);
    }
    *h.finalize().as_bytes()
}

pub fn facets(is_module: bool) -> &'static [&'static str] {
    if is_module {
        &MODULE_FACETS
    } else {
        &LEGACY_FACETS
    }
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

/// Lake's source hash ignores line endings (`Hash.ofText` normalizes CRLF to LF).
fn source_hash(bytes: &[u8]) -> String {
    let mut h = blake3::Hasher::new();
    let mut start = 0;
    for i in 0..bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            h.update(&bytes[start..i]);
            start = i + 1;
        }
    }
    h.update(&bytes[start..]);
    h.finalize().to_hex().to_string()
}

fn module_file(lib: &Lib, comps: &[String]) -> PathBuf {
    let mut p = lib.src_dir.clone();
    for c in &comps[..comps.len() - 1] {
        p.push(c);
    }
    p.push(format!("{}.lean", comps[comps.len() - 1]));
    p
}

/// The modules a library's globs select, as Lake's `Glob.forEachModuleIn`: the base of
/// `A` and `A.*` is included whether or not its file exists (a missing file fails to
/// compile), and the directory of `A.+`/`A.*` must exist.
fn lib_modules(lib: &Lib) -> Result<Vec<Vec<String>>, String> {
    let mut out = Vec::new();
    for g in &lib.globs {
        let (base, me, subs) = match g {
            Glob::One(n) => (n, true, false),
            Glob::Submodules(n) => (n, false, true),
            Glob::AndSubmodules(n) => (n, true, true),
        };
        if me {
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

/// Lean's `forEachModuleInDir`: directories (following symlinks) are recursed into, and
/// every file with extension `lean` is a module named by its file stem.
fn walk(dir: &Path, prefix: &[String], out: &mut Vec<Vec<String>>) -> Result<(), String> {
    let rd = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read directory {}: {e}", dir.display()))?;
    for e in rd {
        let e = e.map_err(|e| e.to_string())?;
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if std::fs::metadata(&path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            let mut p = prefix.to_vec();
            p.push(name);
            walk(&path, &p, out)?;
        } else if path.extension().is_some_and(|x| x == "lean") {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut p = prefix.to_vec();
            p.push(stem);
            out.push(p);
        }
    }
    Ok(())
}

struct Graph {
    mods: Vec<Module>,
}

/// The targets to build, and the errors of library targets whose modules could not be
/// listed (Lake fails those and still builds the rest); `None` when there are no targets
/// (Lake then warns and does nothing).
fn target_roots(pkg: &Package, targets: &[String]) -> Result<Option<(Roots, Vec<String>)>, String> {
    let names: Vec<String> = if !targets.is_empty() {
        targets.to_vec()
    } else {
        pkg.default_targets.clone()
    };
    if names.is_empty() {
        return Ok(None);
    }
    let mut roots = Vec::new();
    let mut errors = Vec::new();
    for t in &names {
        if let Some(li) = pkg.libs.iter().position(|l| &l.name == t) {
            match lib_modules(&pkg.libs[li]) {
                Ok(ms) => roots.extend(ms.into_iter().map(|m| (m, li))),
                Err(e) => errors.push(format!("{t}: {e}")),
            }
        } else {
            let comps = crate::config::split_name(t);
            match pkg.find_module(&comps) {
                Some(li) => roots.push((comps, li)),
                None => return Err(format!("unknown target `{t}`")),
            }
        }
    }
    Ok(Some((roots, errors)))
}

fn read_source(file: &Path, stat: FileStat) -> Result<SourceInfo, String> {
    let bytes = std::fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let h = header::parse(&text).map_err(|e| {
        let line = text[..e.pos.min(text.len())].matches('\n').count() + 1;
        format!("{}:{}: {}", file.display(), line, e.msg)
    })?;
    let racy = stat.is_racy();
    Ok(SourceInfo {
        stat,
        hash: source_hash(&bytes),
        is_module: h.is_module,
        imports: h
            .imports
            .into_iter()
            .map(|i| StoredImport {
                module: i.module,
                all: i.all,
                exported: i.exported,
                meta: i.meta,
            })
            .collect(),
        racy,
    })
}

fn discover(
    pkg: &Package,
    state: &State,
    roots: Roots,
    sources: &mut HashMap<String, SourceInfo>,
) -> Graph {
    let mut index: HashMap<Vec<String>, usize> = HashMap::new();
    let mut mods: Vec<Module> = Vec::new();
    let mut imports_of: Vec<Vec<StoredImport>> = Vec::new();
    let mut queue: VecDeque<(Vec<String>, usize)> = roots.into_iter().collect();
    while let Some((comps, li)) = queue.pop_front() {
        if index.contains_key(&comps) {
            continue;
        }
        let file = module_file(&pkg.libs[li], &comps);
        let name = header::name_to_string(&comps);
        let info = match FileStat::of(&file) {
            Err(e) => Err(format!("cannot read {}: {e}", file.display())),
            Ok(stat) => match state.sources.get(&name) {
                Some(s) if s.stat == stat && !s.racy => Ok(s.clone()),
                _ => read_source(&file, stat),
            },
        };
        let rel_file = file
            .strip_prefix(&pkg.root)
            .unwrap_or(&file)
            .to_string_lossy()
            .to_string();
        let (is_module, src_hash, error, imps) = match info {
            Ok(info) => {
                let imps = info.imports.clone();
                let r = (info.is_module, info.hash.clone(), None, imps);
                sources.insert(name.clone(), info);
                r
            }
            Err(e) => (false, String::new(), Some(e), Vec::new()),
        };
        for imp in &imps {
            if !index.contains_key(&imp.module) {
                if let Some(ili) = pkg.find_module(&imp.module) {
                    queue.push_back((imp.module.clone(), ili));
                }
            }
        }
        index.insert(comps.clone(), mods.len());
        mods.push(Module {
            name,
            comps,
            file,
            rel_file,
            lib: li,
            is_module,
            src_hash,
            error,
            imports: Vec::new(),
            deps: Vec::new(),
            dependents: Vec::new(),
            height: 0,
        });
        imports_of.push(imps);
    }
    for (i, imps) in imports_of.iter().enumerate() {
        mods[i].imports = imps
            .iter()
            .filter_map(|imp| {
                index.get(&imp.module).map(|&idx| LocalImport {
                    idx,
                    all: imp.all,
                    exported: imp.exported,
                    meta: imp.meta,
                })
            })
            .collect();
        let mut deps: Vec<usize> = mods[i].imports.iter().map(|li| li.idx).collect();
        deps.sort_unstable();
        deps.dedup();
        mods[i].deps = deps;
    }
    // Modules on an import cycle fail on their own; their dependents are blocked.
    let n = mods.len();
    let cyclic = cycle_members(&mods);
    for &i in &cyclic {
        mods[i].error = Some(format!("import cycle involving `{}`", mods[i].name));
        mods[i].imports.clear();
        mods[i].deps.clear();
    }
    for i in 0..n {
        for d in mods[i].deps.clone() {
            mods[d].dependents.push(i);
        }
    }
    // heights, in reverse topological order
    let mut indeg: Vec<usize> = mods.iter().map(|m| m.deps.len()).collect();
    let mut order: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut k = 0;
    while k < order.len() {
        let i = order[k];
        k += 1;
        for j in mods[i].dependents.clone() {
            indeg[j] -= 1;
            if indeg[j] == 0 {
                order.push(j);
            }
        }
    }
    for &i in order.iter().rev() {
        let h = mods[i]
            .dependents
            .iter()
            .map(|&j| mods[j].height + 1)
            .max()
            .unwrap_or(0);
        mods[i].height = h;
    }
    Graph { mods }
}

/// Modules that lie on an import cycle (strongly connected components of size > 1, or
/// a module importing itself), found with Tarjan's algorithm (iteratively).
fn cycle_members(mods: &[Module]) -> Vec<usize> {
    let n = mods.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next = 0usize;
    let mut out = Vec::new();
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&(v, ei)) = work.last() {
            if ei == 0 && index[v] == usize::MAX {
                index[v] = next;
                low[v] = next;
                next += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if ei < mods[v].deps.len() {
                work.last_mut().unwrap().1 += 1;
                let w = mods[v].deps[ei];
                if index[w] == usize::MAX {
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    low[parent] = low[parent].min(low[v]);
                }
                if low[v] == index[v] {
                    let mut comp = Vec::new();
                    loop {
                        let w = stack.pop().unwrap();
                        on_stack[w] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    if comp.len() > 1 || mods[v].deps.contains(&v) {
                        out.extend(comp);
                    }
                }
            }
        }
    }
    out
}

/// Lake's `arts` / `allArts` of a module, as paths.
fn import_arts(pkg: &Package, m: &Module, all: bool) -> Vec<Vec<String>> {
    let p = |f: &str| {
        artifact_path(pkg, &m.comps, f)
            .to_string_lossy()
            .to_string()
    };
    if !m.is_module {
        vec![vec![p("olean")]]
    } else if all {
        vec![
            vec![p("olean"), p("olean.server"), p("olean.private")],
            vec![p("ir.sig"), p("ir")],
        ]
    } else {
        vec![
            vec![p("olean"), p("olean.server")],
            vec![p("ir.sig"), p("ir")],
        ]
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
                q.push(Entry {
                    idx: imp.idx,
                    all: ia,
                    meta: nm,
                });
            }
        }
    };
    let mut q: Vec<Entry> = Vec::new();
    for li in m.imports.iter().rev() {
        enqueue(non_module || li.all, li.meta, li.idx, &mut q);
    }
    let mut all_visited: HashSet<usize> = HashSet::new();
    let mut meta_visited: HashSet<usize> = HashSet::new();
    while let Some(Entry { idx, all, meta }) = q.pop() {
        let do_all = all && !all_visited.contains(&idx);
        let do_meta = meta && !meta_visited.contains(&idx);
        let existing = pos.get(&idx).copied();
        // only a module-system entry (which carries `.server`) is ever widened
        if existing.is_some() && !(g.mods[idx].is_module && (do_all || do_meta)) {
            continue;
        }
        if all {
            all_visited.insert(idx);
        }
        if meta {
            meta_visited.insert(idx);
        }
        let had_all = existing.is_some_and(|k| s[k].1 && g.mods[idx].is_module);
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
        put(serde_json::to_string(&lib.options)
            .unwrap_or_default()
            .as_bytes());
        put(lib.more_args.join("\0").as_bytes()); // Lake traces `leanArgs`, not the weak ones
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
        let out = |f: &str| {
            outputs
                .iter()
                .find(|(k, _)| k == f)
                .map(|(_, v)| v.as_bytes())
                .unwrap_or(b"")
        };
        let (arts, meta_arts, all_arts) = if m.is_module {
            (
                mix(&[b"arts", out("olean")]),
                mix(&[b"meta", out("olean"), out("ir.sig"), out("ir")]),
                mix(&[
                    b"all",
                    out("olean"),
                    out("olean.server"),
                    out("olean.private"),
                    out("ir.sig"),
                    out("ir"),
                ]),
            )
        } else {
            (
                mix(&[b"arts", out("olean")]),
                mix(&[b"meta", out("olean")]),
                mix(&[b"all", out("olean")]),
            )
        };
        let (mut trans, mut meta_trans, mut all_trans, mut legacy_trans) = (
            blake3::Hasher::new(),
            blake3::Hasher::new(),
            blake3::Hasher::new(),
            blake3::Hasher::new(),
        );
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
                let (t, a) = if li.meta {
                    (&d.meta_trans, &d.meta_arts)
                } else {
                    (&d.trans, &d.arts)
                };
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

    /// The remembered outputs if the module is up to date: every output exists with its
    /// remembered status, or with its remembered content (then the record is refreshed,
    /// e.g. in a copied checkout).
    fn up_to_date(&self, i: usize, key: &str) -> Option<(Outputs, Option<Built>)> {
        let m = &self.g.mods[i];
        let b = self.state.built.get(&m.name)?;
        if b.key != key || b.outputs.len() != facets(m.is_module).len() {
            return None;
        }
        let mut stats = Vec::with_capacity(b.outputs.len());
        let mut changed = false;
        for (f, h) in &b.outputs {
            let p = artifact_path(self.pkg, &m.comps, f);
            let stat = FileStat::of(&p).ok()?;
            let remembered = b.stats.iter().find(|(k, _)| k == f).map(|(_, s)| s);
            if remembered != Some(&stat) {
                if hash_file(&p).ok()? != *h {
                    return None;
                }
                changed = true;
            }
            stats.push((f.clone(), stat));
        }
        let refreshed = changed.then(|| Built {
            key: key.to_string(),
            outputs: b.outputs.clone(),
            stats,
        });
        Some((b.outputs.clone(), refreshed))
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

    /// Compile a module: `Ok((outputs, stats, messages, ms))` or `Err((messages, error))`.
    #[allow(clippy::type_complexity)]
    fn compile(
        &self,
        i: usize,
    ) -> Result<(Outputs, Vec<(String, FileStat)>, Vec<String>, u128), (Vec<String>, String)> {
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
        // Like Lake (`Module.clearOutputArtifacts`): remove the old outputs first, so a
        // failed compile never leaves stale artifacts behind.
        for f in MODULE_FACETS.iter().chain(["ltar", "bc"].iter()) {
            let _ = std::fs::remove_file(artifact_path(self.pkg, &m.comps, f));
        }
        if let Err(e) = std::fs::write(
            &setup,
            serde_json::to_vec_pretty(&self.setup_json(i)).unwrap_or_default(),
        ) {
            return Err((vec![], format!("cannot write {}: {e}", setup.display())));
        }
        let mut cmd = Command::new(&self.tc.lean);
        cmd.args(&lib.weak_args)
            .args(&lib.more_args)
            .arg(&m.file)
            .arg("-o")
            .arg(&olean)
            .arg("-i")
            .arg(&ilean)
            .arg("-c")
            .arg(&c)
            .arg("--setup")
            .arg(&setup)
            .arg("--json")
            .env("LEAN_PATH", &self.pkg.lib_dir)
            .current_dir(&self.pkg.root)
            .stdin(Stdio::null());
        let t0 = Instant::now();
        let out = cmd.output().map_err(|e| {
            (
                vec![],
                format!("cannot run {}: {e}", self.tc.lean.display()),
            )
        })?;
        let ms = t0.elapsed().as_millis();
        let mut msgs = Vec::new();
        let mut logged_error = false;
        for line in String::from_utf8_lossy(&out.stdout)
            .lines()
            .chain(String::from_utf8_lossy(&out.stderr).lines())
        {
            if line.trim().is_empty() {
                continue;
            }
            let (text, is_error) = render_message(line, &m.rel_file);
            logged_error |= is_error;
            msgs.push(text);
        }
        if !out.status.success() || logged_error {
            return Err((msgs, String::new()));
        }
        let mut outputs = Vec::new();
        let mut stats = Vec::new();
        for f in facets(m.is_module) {
            let p = artifact_path(self.pkg, &m.comps, f);
            match (hash_file(&p), FileStat::of(&p)) {
                (Ok(h), Ok(st)) => {
                    outputs.push((f.to_string(), h));
                    stats.push((f.to_string(), st));
                }
                (Err(e), _) | (_, Err(e)) => {
                    return Err((msgs, format!("missing output {}: {e}", p.display())))
                }
            }
        }
        Ok((outputs, stats, msgs, ms))
    }

    fn print(&self, text: &str) {
        let _g = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let mut so = std::io::stdout().lock();
        let _ = so.write_all(text.as_bytes());
        let _ = so.flush();
    }

    fn next_count(&self) -> usize {
        let mut s = self.sched.lock().unwrap_or_else(|e| e.into_inner());
        s.compiled += 1;
        s.compiled
    }

    fn report_failure(&self, name: &str, msgs: &[String], err: &str) {
        let n = self.next_count();
        let mut text = format!("✖ [{n}/{}] Building {name}\n", self.g.mods.len());
        for l in msgs {
            text.push_str(l);
            text.push('\n');
        }
        if !err.is_empty() {
            text.push_str(&format!("error: {err}\n"));
        }
        self.print(&text);
    }

    /// Check or build one module whose dependencies are all finished.
    fn process(&self, i: usize) -> Outcome {
        let m = &self.g.mods[i];
        let total = self.g.mods.len();
        if let Some(e) = &m.error {
            self.report_failure(&m.name, &[], e);
            return (Status::Failed, Vec::new(), Some(None));
        }
        let key = {
            let s = self.sched.lock().unwrap_or_else(|e| e.into_inner());
            self.key(i, &s.done)
        };
        if let Some((outputs, refreshed)) = self.up_to_date(i, &key) {
            return (Status::UpToDate, outputs, refreshed.map(Some));
        }
        match self.compile(i) {
            Ok((outputs, stats, msgs, ms)) => {
                let n = self.next_count();
                let warn = msgs.iter().any(|l| l.starts_with("warning:"));
                let mut text = format!(
                    "{} [{n}/{total}] Built {} ({ms}ms)\n",
                    if warn { "⚠" } else { "✔" },
                    m.name
                );
                if warn || self.verbose {
                    for l in &msgs {
                        text.push_str(l);
                        text.push('\n');
                    }
                }
                self.print(&text);
                let b = Built {
                    key,
                    outputs: outputs.clone(),
                    stats,
                };
                (Status::Built, outputs, Some(Some(b)))
            }
            Err((msgs, err)) => {
                self.report_failure(&m.name, &msgs, &err);
                (Status::Failed, Vec::new(), Some(None))
            }
        }
    }

    fn worker(&self) {
        let total = self.g.mods.len();
        loop {
            let i = {
                let mut s = self.sched.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some((_, std::cmp::Reverse(i))) = s.ready.pop() {
                        break i;
                    }
                    if s.finished == total {
                        return;
                    }
                    s = self.cv.wait(s).unwrap_or_else(|e| e.into_inner());
                }
            };
            // a panic while processing one module fails that module, not the build
            let (status, outputs, built) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.process(i)))
                    .unwrap_or_else(|_| {
                        self.report_failure(
                            &self.g.mods[i].name,
                            &[],
                            "internal error (panic) in flume",
                        );
                        (Status::Failed, Vec::new(), Some(None))
                    });
            let mut s = self.sched.lock().unwrap_or_else(|e| e.into_inner());
            if status != Status::Failed {
                let info = self.export_info(i, &outputs, &s.done);
                s.done[i] = info;
            }
            s.status[i] = status;
            if let Some(b) = built {
                s.new_built.insert(self.g.mods[i].name.clone(), b);
            }
            s.finished += 1;
            let mut stack = vec![i];
            while let Some(j) = stack.pop() {
                let bad = matches!(s.status[j], Status::Failed | Status::Blocked);
                for &d in &self.g.mods[j].dependents {
                    s.pending[d] -= 1;
                    if s.pending[d] == 0 {
                        let blocked = bad
                            || self.g.mods[d]
                                .deps
                                .iter()
                                .any(|&x| matches!(s.status[x], Status::Failed | Status::Blocked));
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

/// Lake's rendering of one `lean --json` message, and whether it is an error.
fn render_message(line: &str, rel_file: &str) -> (String, bool) {
    match serde_json::from_str::<Json>(line) {
        Ok(Json::Object(o)) => {
            let sev = o.get("severity").and_then(Json::as_str).unwrap_or("info");
            let is_error = sev == "error";
            let sev = if sev == "information" { "info" } else { sev };
            let data = o
                .get("data")
                .and_then(Json::as_str)
                .unwrap_or("")
                .trim_end();
            let text = match o.get("pos") {
                Some(p) => format!(
                    "{sev}: {rel_file}:{}:{}: {data}",
                    p.get("line").and_then(Json::as_u64).unwrap_or(0),
                    p.get("column").and_then(Json::as_u64).unwrap_or(0)
                ),
                None => format!("{sev}: {rel_file}: {data}"),
            };
            (text, is_error)
        }
        _ => (line.to_string(), false),
    }
}

/// Build the targets; returns whether everything succeeded.
pub fn build(pkg: &Package, tc: &Toolchain, opts: &Options) -> Result<bool, String> {
    let Some((roots, target_errors)) = target_roots(pkg, &opts.targets)? else {
        // Lake's behavior (and wording) when nothing is selected
        println!(
            "warning: no targets specified and no default targets configured\n  \
             Note: This will be an error in a future version of Lake.\n  \
             Hint: This warning (or error) can be suppressed with '--allow-empty'."
        );
        return Ok(true);
    };
    let flume_dir = pkg.root.join(".lake").join("flume");
    let _lock = BuildLock::acquire(&flume_dir.join("lock"))
        .map_err(|e| format!("cannot lock {}: {e}", flume_dir.display()))?;
    let state_path = flume_dir.join("state.json");
    let state = State::load(&state_path, &tc.githash);
    let mut sources = HashMap::new();
    let g = discover(pkg, &state, roots, &mut sources);
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
    let s = shared.sched.into_inner().unwrap_or_else(|e| e.into_inner());
    for e in &target_errors {
        println!("✖ Collecting modules\nerror: {e}");
    }
    let failed: Vec<&str> = (0..n)
        .filter(|&i| s.status[i] == Status::Failed)
        .map(|i| g.mods[i].name.as_str())
        .collect();
    // persist what changed
    let sources_changed = sources.iter().any(|(k, v)| {
        state
            .sources
            .get(k)
            .is_none_or(|o| o.stat != v.stat || o.racy != v.racy)
    });
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
        new_state
            .save(&state_path)
            .map_err(|e| format!("cannot write {}: {e}", state_path.display()))?;
    }
    if failed.is_empty() && target_errors.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_is_ignored_in_source_hashes() {
        assert_eq!(source_hash(b"a\r\nb\r\n"), source_hash(b"a\nb\n"));
        assert_ne!(source_hash(b"a\rb"), source_hash(b"a\nb"));
    }

    fn module(deps: Vec<usize>) -> Module {
        Module {
            name: String::new(),
            comps: vec![],
            file: PathBuf::new(),
            rel_file: String::new(),
            lib: 0,
            is_module: true,
            src_hash: String::new(),
            error: None,
            imports: vec![],
            deps,
            dependents: vec![],
            height: 0,
        }
    }

    #[test]
    fn cycles() {
        // 0 <- 1 <- 2 <- 1 (cycle 1,2), 3 imports itself, 4 imports 2
        let mods = vec![
            module(vec![]),
            module(vec![0, 2]),
            module(vec![1]),
            module(vec![3]),
            module(vec![2]),
        ];
        let mut c = cycle_members(&mods);
        c.sort();
        assert_eq!(c, vec![1, 2, 3]);
    }
}
