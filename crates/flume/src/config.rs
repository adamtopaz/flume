//! Reading a package's `lakefile.toml`.
//!
//! Version 0 supports the subset the benchmark corpora and simple projects use: the
//! package `name`, `defaultTargets`, `srcDir`, `buildDir`, `leanLibDir`, `irDir`,
//! `leanOptions`, `moreLeanArgs`, `weakLeanArgs`, and `[[lean_lib]]` entries with
//! `name`, `srcDir`, `roots`, `globs`, `leanOptions`, `moreLeanArgs`, `weakLeanArgs`.
//! Anything that would change what gets built and is not supported yet (`[[require]]`,
//! `[[lean_exe]]` targets, `lakefile.lean`, options that split compilation) is an error,
//! never silently ignored.

use serde_json::{Map, Value as Json};
use std::path::{Path, PathBuf};
use toml::Value;

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
    pub fn parse(s: &str) -> Glob {
        if let Some(p) = s.strip_suffix(".+") {
            Glob::Submodules(split_name(p))
        } else if let Some(p) = s.strip_suffix(".*") {
            Glob::AndSubmodules(split_name(p))
        } else {
            Glob::One(split_name(s))
        }
    }
    pub fn matches(&self, m: &[String]) -> bool {
        match self {
            Glob::One(n) => m == n.as_slice(),
            Glob::Submodules(n) => m.len() > n.len() && m.starts_with(n),
            Glob::AndSubmodules(n) => m.starts_with(n),
        }
    }
}

pub fn split_name(s: &str) -> Vec<String> {
    s.split('.').map(|c| c.trim_matches(|ch| ch == '«' || ch == '»').to_string()).collect()
}

#[derive(Clone, Debug)]
pub struct Lib {
    pub name: String,
    pub src_dir: PathBuf,
    pub roots: Vec<Vec<String>>,
    pub globs: Vec<Glob>,
    /// Lean options (package's, overridden by the library's), passed in the setup file.
    pub options: Map<String, Json>,
    /// `weakLeanArgs ++ moreLeanArgs` (package's, then the library's), before the file.
    pub lean_args: Vec<String>,
}

impl Lib {
    /// Lake's `LeanLib.isLocalModule`.
    pub fn is_local_module(&self, m: &[String]) -> bool {
        self.roots.iter().any(|r| m.starts_with(r)) || self.globs.iter().any(|g| g.matches(m))
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

fn strings(v: Option<&Value>, what: &str) -> Result<Vec<String>, String> {
    match v {
        None => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| x.as_str().map(str::to_string).ok_or_else(|| format!("`{what}` must be a list of strings")))
            .collect(),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(_) => Err(format!("`{what}` must be a list of strings")),
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

fn flatten_options(prefix: &str, t: &toml::Table, out: &mut Map<String, Json>) -> Result<(), String> {
    for (k, v) in t {
        let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match v {
            Value::Table(sub) => flatten_options(&key, sub, out)?,
            Value::Boolean(b) => {
                out.insert(key, Json::Bool(*b));
            }
            Value::Integer(i) => {
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

pub fn load(root: &Path) -> Result<Package, String> {
    let toml_path = root.join("lakefile.toml");
    if !toml_path.is_file() {
        if root.join("lakefile.lean").is_file() {
            return Err("this project is configured in lakefile.lean, which flume does not read yet \
                        (roadmap milestone M2); only lakefile.toml is supported"
                .into());
        }
        return Err(format!("no lakefile.toml in {}", root.display()));
    }
    let text = std::fs::read_to_string(&toml_path).map_err(|e| format!("cannot read {}: {e}", toml_path.display()))?;
    let t: toml::Table = text.parse().map_err(|e| format!("{}: {e}", toml_path.display()))?;
    let name = t.get("name").and_then(Value::as_str).ok_or("lakefile.toml has no package `name`")?.to_string();
    if t.get("require").is_some() {
        return Err("dependencies (`[[require]]`) are not supported yet (roadmap milestone M2)".into());
    }
    let src_dir = root.join(str_or(&t, "srcDir", ".")?);
    let build_dir = root.join(str_or(&t, "buildDir", ".lake/build")?);
    let lib_dir = build_dir.join(str_or(&t, "leanLibDir", "lib/lean")?);
    let ir_dir = build_dir.join(str_or(&t, "irDir", "ir")?);
    let pkg_options = options(t.get("leanOptions"))?;
    let pkg_args = [strings(t.get("weakLeanArgs"), "weakLeanArgs")?, strings(t.get("moreLeanArgs"), "moreLeanArgs")?].concat();
    for key in ["precompileModules", "platformIndependent", "moreLeancArgs", "moreLinkArgs", "extraDepTargets"] {
        if t.get(key).is_some() {
            return Err(format!("package option `{key}` is not supported yet"));
        }
    }
    let mut libs = Vec::new();
    if let Some(v) = t.get("lean_lib") {
        let arr = v.as_array().ok_or("`lean_lib` must be an array of tables")?;
        for l in arr {
            let lt = l.as_table().ok_or("`lean_lib` entries must be tables")?;
            let lname = lt.get("name").and_then(Value::as_str).ok_or("a `lean_lib` has no `name`")?.to_string();
            let mut roots: Vec<Vec<String>> = strings(lt.get("roots"), "roots")?.iter().map(|s| split_name(s)).collect();
            if roots.is_empty() {
                roots.push(split_name(&lname));
            }
            let mut globs: Vec<Glob> = strings(lt.get("globs"), "globs")?.iter().map(|s| Glob::parse(s)).collect();
            if globs.is_empty() {
                globs = roots.iter().cloned().map(Glob::One).collect();
            }
            let mut opts = pkg_options.clone();
            opts.extend(options(lt.get("leanOptions"))?);
            if opts.contains_key("compiler.postponeCompile") {
                return Err("the Lean option `compiler.postponeCompile` is not supported yet".into());
            }
            let lib_src = match lt.get("srcDir") {
                Some(Value::String(s)) => src_dir.join(s),
                Some(_) => return Err("`srcDir` must be a string".into()),
                None => src_dir.clone(),
            };
            let lean_args = [
                pkg_args.clone(),
                strings(lt.get("weakLeanArgs"), "weakLeanArgs")?,
                strings(lt.get("moreLeanArgs"), "moreLeanArgs")?,
            ]
            .concat();
            for key in ["precompileModules", "extraDepTargets", "needs", "nativeFacets"] {
                if lt.get(key).is_some() {
                    return Err(format!("`lean_lib` option `{key}` is not supported yet"));
                }
            }
            libs.push(Lib { name: lname, src_dir: lib_src, roots, globs, options: opts, lean_args });
        }
    }
    let exes: Vec<String> = match t.get("lean_exe") {
        Some(Value::Array(a)) => a.iter().filter_map(|e| e.get("name").and_then(Value::as_str).map(str::to_string)).collect(),
        _ => Vec::new(),
    };
    let default_targets = strings(t.get("defaultTargets"), "defaultTargets")?;
    for target in &default_targets {
        if exes.contains(target) {
            return Err(format!("default target `{target}` is an executable; `lean_exe` is not supported yet (roadmap milestone M7)"));
        }
    }
    Ok(Package { name, root: root.to_path_buf(), build_dir, lib_dir, ir_dir, default_targets, libs })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        let m = split_name("A.B.C");
        assert!(Glob::parse("A.B.+").matches(&m));
        assert!(Glob::parse("A.*").matches(&m));
        assert!(!Glob::parse("A.B.C.+").matches(&m));
        assert!(Glob::parse("A.B.C.*").matches(&m));
        assert!(Glob::parse("A.B.C").matches(&m));
    }
}
