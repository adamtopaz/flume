//! Reading a package's configuration: `lakefile.toml`, or `lakefile.lean` through Lake's
//! own loader (`lean_config.rs`), which wins when both exist, as in Lake.
//!
//! Version 0 supports the subset the benchmark corpora and simple projects use: the
//! package `name`, `defaultTargets`, `srcDir`, `buildDir`, `leanLibDir`, `irDir`,
//! `leanOptions`, `moreLeanArgs`, `weakLeanArgs`, and `[[lean_lib]]` entries with
//! `name`, `srcDir`, `roots`, `globs`, `leanOptions`, `moreLeanArgs`, `weakLeanArgs`,
//! `defaultFacets = ["leanArts"]`. Keys that cannot change a library's Lean artifacts
//! (metadata, linking, server, cache options) are accepted. Every other key is an error,
//! never silently ignored: dependencies, `lean_exe` targets, a non-release `buildType`,
//! `bootstrap`, plugins, precompilation, and anything unknown.

use serde_json::{Map, Value as Json};
use std::path::{Path, PathBuf};
use toml::Value;

/// Package keys flume implements.
const PKG_KEYS: &[&str] = &[
    "name",
    "defaultTargets",
    "srcDir",
    "buildDir",
    "leanLibDir",
    "irDir",
    "leanOptions",
    "moreLeanArgs",
    "weakLeanArgs",
    "lean_lib",
    "lean_exe",
    "buildType",
    "bootstrap",
];
/// Package keys that cannot change a library's Lean artifacts.
const PKG_INERT: &[&str] = &[
    "version",
    "versionTags",
    "description",
    "keywords",
    "homepage",
    "license",
    "licenseFiles",
    "readmeFile",
    "reservoir",
    "testDriver",
    "testDriverArgs",
    "lintDriver",
    "lintDriverArgs",
    "enableArtifactCache",
    "restoreAllArtifacts",
    "libPrefixOnWindows",
    "binDir",
    "nativeLibDir",
    "releaseRepo",
    "buildArchive",
    "preferReleaseBuild",
    "platformIndependent",
    "moreLinkArgs",
    "moreLinkObjs",
    "moreLinkLibs",
    "moreLeancArgs",
    "weakLeancArgs",
    "moreServerArgs",
    "moreGlobalServerArgs",
];
/// Library keys flume implements.
const LIB_KEYS: &[&str] = &[
    "name",
    "srcDir",
    "roots",
    "globs",
    "leanOptions",
    "moreLeanArgs",
    "weakLeanArgs",
    "defaultFacets",
    "buildType",
];
/// Library keys that cannot change its Lean artifacts.
const LIB_INERT: &[&str] = &[
    "libName",
    "libPrefixOnWindows",
    "platformIndependent",
    "moreLinkArgs",
    "moreLinkObjs",
    "moreLinkLibs",
    "moreLeancArgs",
    "weakLeancArgs",
    "moreServerArgs",
];

/// A Lake module glob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Glob {
    /// `A.B`: exactly that module
    One(Vec<String>),
    /// `A.B.+`: its strict submodules
    Submodules(Vec<String>),
    /// `A.B.*`: it and its submodules
    AndSubmodules(Vec<String>),
}

impl Glob {
    pub fn parse(s: &str) -> Result<Glob, String> {
        let g = if let Some(p) = s.strip_suffix(".+") {
            Glob::Submodules(parse_name(p)?)
        } else if let Some(p) = s.strip_suffix(".*") {
            Glob::AndSubmodules(parse_name(p)?)
        } else {
            Glob::One(parse_name(s)?)
        };
        Ok(g)
    }
    pub fn matches(&self, m: &[String]) -> bool {
        match self {
            Glob::One(n) => m == n.as_slice(),
            Glob::Submodules(n) => m.len() > n.len() && m.starts_with(n),
            Glob::AndSubmodules(n) => m.starts_with(n),
        }
    }
}

/// Parse a Lean name written with dots and optional `«»` escapes (`A.«b.c».D`).
pub fn parse_name(s: &str) -> Result<Vec<String>, String> {
    let mut comps = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    let mut escaped_part = false;
    while let Some(c) = chars.next() {
        match c {
            '«' => {
                let mut part = String::new();
                loop {
                    match chars.next() {
                        Some('»') => break,
                        Some(d) => part.push(d),
                        None => return Err(format!("unterminated `«` in name `{s}`")),
                    }
                }
                cur.push_str(&part);
                escaped_part = true;
            }
            '.' => {
                if cur.is_empty() && !escaped_part {
                    return Err(format!("malformed name `{s}`"));
                }
                comps.push(std::mem::take(&mut cur));
                escaped_part = false;
            }
            _ => cur.push(c),
        }
    }
    if cur.is_empty() && !escaped_part {
        return Err(format!("malformed name `{s}`"));
    }
    comps.push(cur);
    Ok(comps)
}

/// Like `parse_name` for names flume reads from trusted places (command line targets).
pub fn split_name(s: &str) -> Vec<String> {
    parse_name(s).unwrap_or_else(|_| vec![s.to_string()])
}

#[derive(Clone, Debug)]
pub struct Lib {
    pub name: String,
    pub src_dir: PathBuf,
    pub roots: Vec<Vec<String>>,
    pub globs: Vec<Glob>,
    /// Lean options (package's, overridden by the library's), passed in the setup file.
    pub options: Map<String, Json>,
    /// `weakLeanArgs` (package's, then the library's): first on the command line, not traced.
    pub weak_args: Vec<String>,
    /// `moreLeanArgs` (package's, then the library's): after the weak ones, traced.
    pub more_args: Vec<String>,
}

impl Lib {
    /// Lake's `LeanLibConfig.isBuildableModule`.
    pub fn is_buildable_module(&self, m: &[String]) -> bool {
        self.globs.iter().any(|g| g.matches(m))
            || self
                .roots
                .iter()
                .any(|r| m.starts_with(r) && self.globs.iter().any(|g| g.matches(r)))
    }
}

#[derive(Clone, Debug)]
pub struct Package {
    pub name: String,
    pub root: PathBuf,
    pub build_dir: PathBuf,
    /// where `.olean`, `.ilean`, `.ir` files go (`<buildDir>/lib/lean` by default)
    pub lib_dir: PathBuf,
    /// where `.c` and setup files go (`<buildDir>/ir` by default)
    pub ir_dir: PathBuf,
    pub default_targets: Vec<String>,
    pub libs: Vec<Lib>,
}

impl Package {
    /// Lake's `Package.findModule?`: the last library that can build the module.
    pub fn find_module(&self, m: &[String]) -> Option<usize> {
        self.libs.iter().rposition(|l| l.is_buildable_module(m))
    }
}

fn strings(v: Option<&Value>, what: &str) -> Result<Vec<String>, String> {
    match v {
        None => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("`{what}` must be an array of strings"))
            })
            .collect(),
        Some(_) => Err(format!("`{what}` must be an array of strings")),
    }
}

fn options(v: Option<&Value>) -> Result<Map<String, Json>, String> {
    let mut out = Map::new();
    if let Some(v) = v {
        let t = v.as_table().ok_or("`leanOptions` must be a table")?;
        flatten_options("", t, &mut out)?;
    }
    Ok(out)
}

/// One component of an option name: a TOML key containing a dot (only possible when
/// quoted) is a single component, which Lake renders escaped.
fn option_component(k: &str) -> String {
    if k.contains('.') {
        format!("«{k}»")
    } else {
        k.to_string()
    }
}

fn flatten_options(
    prefix: &str,
    t: &toml::Table,
    out: &mut Map<String, Json>,
) -> Result<(), String> {
    for (k, v) in t {
        let comp = option_component(k);
        let key = if prefix.is_empty() {
            comp
        } else {
            format!("{prefix}.{comp}")
        };
        match v {
            Value::Table(sub) => flatten_options(&key, sub, out)?,
            Value::Boolean(b) => {
                out.insert(key, Json::Bool(*b));
            }
            Value::Integer(i) if *i >= 0 => {
                out.insert(key, Json::from(*i));
            }
            Value::String(s) => {
                out.insert(key, Json::String(s.clone()));
            }
            _ => return Err(format!("unsupported value for Lean option `{key}`")),
        }
    }
    Ok(())
}

fn str_or<'a>(t: &'a toml::Table, key: &str, default: &'a str) -> Result<&'a str, String> {
    match t.get(key) {
        None => Ok(default),
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn check_keys(t: &toml::Table, known: &[&str], inert: &[&str], what: &str) -> Result<(), String> {
    for k in t.keys() {
        if k == "require" {
            return Err(
                "dependencies (`[[require]]`) are not supported yet (roadmap milestone C1)".into(),
            );
        }
        if !known.contains(&k.as_str()) && !inert.contains(&k.as_str()) {
            return Err(format!("{what} option `{k}` is not supported by flume yet"));
        }
    }
    if let Some(bt) = t.get("buildType") {
        if bt.as_str() != Some("release") {
            return Err(format!(
                "{what} option `buildType` other than \"release\" is not supported yet"
            ));
        }
    }
    if let Some(b) = t.get("bootstrap") {
        if b.as_bool() != Some(false) {
            return Err(format!("{what} option `bootstrap` is not supported yet"));
        }
    }
    Ok(())
}

pub fn load(root: &Path) -> Result<Package, String> {
    // Lake's choice (`Load/Package.lean`): `lakefile.lean` if it exists, else the TOML file
    let lean_path = root.join("lakefile.lean");
    let toml_path = root.join("lakefile.toml");
    let (text, origin) = if lean_path.is_file() {
        let tc = crate::toolchain::detect(root)?;
        (
            crate::lean_config::translate(root, &lean_path, &tc)?,
            lean_path,
        )
    } else if toml_path.is_file() {
        let text = std::fs::read_to_string(&toml_path)
            .map_err(|e| format!("cannot read {}: {e}", toml_path.display()))?;
        (text, toml_path)
    } else {
        return Err(format!(
            "no lakefile.lean or lakefile.toml in {}",
            root.display()
        ));
    };
    let t: toml::Table = text
        .parse()
        .map_err(|e| format!("{} (as TOML): {e}", origin.display()))?;
    check_keys(&t, PKG_KEYS, PKG_INERT, "package")?;
    let name = t
        .get("name")
        .and_then(Value::as_str)
        .ok_or("lakefile.toml has no package `name`")?
        .to_string();
    let src_dir = root.join(str_or(&t, "srcDir", ".")?);
    let build_dir = root.join(str_or(&t, "buildDir", ".lake/build")?);
    let lib_dir = build_dir.join(str_or(&t, "leanLibDir", "lib/lean")?);
    let ir_dir = build_dir.join(str_or(&t, "irDir", "ir")?);
    let pkg_options = options(t.get("leanOptions"))?;
    let pkg_weak = strings(t.get("weakLeanArgs"), "weakLeanArgs")?;
    let pkg_more = strings(t.get("moreLeanArgs"), "moreLeanArgs")?;
    let mut libs = Vec::new();
    if let Some(v) = t.get("lean_lib") {
        let arr = v
            .as_array()
            .ok_or("`lean_lib` must be an array of tables")?;
        for l in arr {
            let lt = l.as_table().ok_or("`lean_lib` entries must be tables")?;
            check_keys(lt, LIB_KEYS, LIB_INERT, "`lean_lib`")?;
            let lname = lt
                .get("name")
                .and_then(Value::as_str)
                .ok_or("a `lean_lib` has no `name`")?
                .to_string();
            let mut roots = Vec::new();
            for s in strings(lt.get("roots"), "roots")? {
                roots.push(parse_name(&s)?);
            }
            if roots.is_empty() {
                roots.push(parse_name(&lname)?);
            }
            let mut globs = Vec::new();
            for s in strings(lt.get("globs"), "globs")? {
                globs.push(Glob::parse(&s)?);
            }
            if globs.is_empty() {
                globs = roots.iter().cloned().map(Glob::One).collect();
            }
            let facets = strings(lt.get("defaultFacets"), "defaultFacets")?;
            if lt.get("defaultFacets").is_some() && facets != ["leanArts"] {
                return Err(format!("`lean_lib` `{lname}`: defaultFacets other than [\"leanArts\"] are not supported yet"));
            }
            let mut opts = pkg_options.clone();
            opts.extend(options(lt.get("leanOptions"))?);
            if opts.contains_key("compiler.postponeCompile") {
                return Err(
                    "the Lean option `compiler.postponeCompile` is not supported yet".into(),
                );
            }
            let lib_src = match lt.get("srcDir") {
                Some(Value::String(s)) => src_dir.join(s),
                Some(_) => return Err("`srcDir` must be a string".into()),
                None => src_dir.clone(),
            };
            libs.push(Lib {
                name: lname,
                src_dir: lib_src,
                roots,
                globs,
                options: opts,
                weak_args: [
                    pkg_weak.clone(),
                    strings(lt.get("weakLeanArgs"), "weakLeanArgs")?,
                ]
                .concat(),
                more_args: [
                    pkg_more.clone(),
                    strings(lt.get("moreLeanArgs"), "moreLeanArgs")?,
                ]
                .concat(),
            });
        }
    }
    let exes: Vec<String> = match t.get("lean_exe") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|e| e.get("name").and_then(Value::as_str).map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let default_targets = strings(t.get("defaultTargets"), "defaultTargets")?;
    for target in &default_targets {
        if exes.contains(target) {
            return Err(format!(
                "default target `{target}` is an executable; `lean_exe` is not supported yet (roadmap milestone C4)"
            ));
        }
    }
    Ok(Package {
        name,
        root: root.to_path_buf(),
        build_dir,
        lib_dir,
        ir_dir,
        default_targets,
        libs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        let m = split_name("A.B.C");
        assert!(Glob::parse("A.B.+").unwrap().matches(&m));
        assert!(Glob::parse("A.*").unwrap().matches(&m));
        assert!(!Glob::parse("A.B.C.+").unwrap().matches(&m));
        assert!(Glob::parse("A.B.C.*").unwrap().matches(&m));
        assert!(Glob::parse("A.B.C").unwrap().matches(&m));
        assert!(Glob::parse("A..B").is_err());
    }

    #[test]
    fn names() {
        assert_eq!(parse_name("A.«b.c».D").unwrap(), vec!["A", "b.c", "D"]);
        assert!(parse_name("A.").is_err());
        assert!(parse_name("«A").is_err());
    }

    #[test]
    fn buildable() {
        let lib = Lib {
            name: "T".into(),
            src_dir: PathBuf::from("."),
            roots: vec![split_name("T")],
            globs: vec![Glob::One(split_name("T"))],
            options: Map::new(),
            weak_args: vec![],
            more_args: vec![],
        };
        assert!(lib.is_buildable_module(&split_name("T.X.Y")));
        let lib2 = Lib {
            globs: vec![Glob::One(split_name("T.A"))],
            ..lib
        };
        assert!(!lib2.is_buildable_module(&split_name("T.X")));
        assert!(lib2.is_buildable_module(&split_name("T.A")));
    }

    #[test]
    fn dotted_option_keys() {
        let t: toml::Table = r#"
            a = { b = true }
            "c.d" = 1
        "#
        .parse()
        .unwrap();
        let mut out = Map::new();
        flatten_options("", &t, &mut out).unwrap();
        assert!(out.contains_key("a.b"));
        assert!(out.contains_key("«c.d»"));
    }
}
