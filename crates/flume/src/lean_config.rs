//! `lakefile.lean` (roadmap C2), evaluated by Lake itself.
//!
//! A Lean configuration is a Lean program, so only Lake can evaluate it faithfully. flume
//! asks the toolchain's `lake` to translate it (`lake translate-config toml`, which loads
//! the root package with Lake's own loader and writes the evaluated, declarative
//! configuration as TOML) and reads the result like a `lakefile.toml`. The translation is
//! cached in `.lake/flume/config/` under a key of the configuration's content, the local
//! files it imports, and the toolchain, so Lake runs only when one of them changes.
//!
//! Lake's translation drops what TOML cannot express ("non-declarative configuration will
//! be discarded"): custom `target`s, `script`s, facets, `extern_lib`s, `post_update` hooks.
//! flume refuses a configuration that declares any of them instead of ignoring them.

use crate::header;
use crate::toolchain::Toolchain;
use std::path::Path;
use std::process::{Command, Stdio};

const CACHE_VERSION: &str = "flume-lean-config-1";

/// Commands of Lake's DSL whose declarations a translation to TOML discards.
const DISCARDED: [&str; 7] = [
    "target",
    "script",
    "extern_lib",
    "module_facet",
    "library_facet",
    "package_facet",
    "post_update",
];

/// The configuration `file` of the package at `root`, as Lake's TOML.
pub fn translate(root: &Path, file: &Path, tc: &Toolchain) -> Result<String, String> {
    let bytes = std::fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    if let Some(cmd) = discarded_command(&text) {
        return Err(format!(
            "{}: flume does not support `{cmd}` declarations yet (roadmap C5); \
             it reads a Lean configuration's declarative part only",
            file.display()
        ));
    }
    let key = cache_key(root, &bytes, &text, tc);
    let dir = root.join(".lake").join("flume").join("config");
    let cached = dir.join(format!("{key}.toml"));
    if let Ok(t) = std::fs::read_to_string(&cached) {
        return Ok(t);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let out = dir.join(format!("translate-{}.toml", std::process::id()));
    let _ = std::fs::remove_file(&out); // Lake refuses to overwrite
    let lake = tc.lean.with_file_name("lake");
    let p = Command::new(&lake)
        .arg("translate-config")
        .arg("toml")
        .arg(&out)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", lake.display()))?;
    let toml = std::fs::read_to_string(&out);
    let _ = std::fs::remove_file(&out);
    let toml = match (p.status.success(), toml) {
        (true, Ok(t)) => t,
        _ => {
            return Err(format!(
                "Lake could not load {}:\n{}{}",
                file.display(),
                String::from_utf8_lossy(&p.stdout),
                String::from_utf8_lossy(&p.stderr)
            ))
        }
    };
    // keep only the current translation
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let tmp = dir.join(format!(".{key}.{}", std::process::id()));
    if std::fs::write(&tmp, &toml).is_ok() {
        let _ = std::fs::rename(&tmp, &cached);
    }
    Ok(toml)
}

/// What the translation depends on: the configuration (as Lake hashes text, line endings
/// normalized), the local files it imports, and the toolchain.
fn cache_key(root: &Path, bytes: &[u8], text: &str, tc: &Toolchain) -> String {
    let mut h = blake3::Hasher::new();
    let mut put = |b: &[u8]| {
        h.update(&(b.len() as u64).to_le_bytes());
        h.update(b);
    };
    put(CACHE_VERSION.as_bytes());
    put(tc.githash.as_bytes());
    put(&normalize(bytes));
    if let Ok(hd) = header::parse(text) {
        for imp in hd.imports {
            if matches!(
                imp.module.first().map(String::as_str),
                Some("Lake" | "Lean" | "Init" | "Std")
            ) {
                continue;
            }
            put(header::name_to_string(&imp.module).as_bytes());
            let mut p = root.to_path_buf();
            for c in &imp.module {
                p.push(c);
            }
            p.set_extension("lean");
            match std::fs::read(&p) {
                Ok(b) => put(&normalize(&b)),
                Err(_) => put(b"<missing>"),
            }
        }
    }
    h.finalize().to_hex().to_string()
}

fn normalize(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        if !(b == b'\r' && bytes.get(i + 1) == Some(&b'\n')) {
            out.push(b);
        }
    }
    out
}

/// The first command of `text` that Lake's translation would discard, if any: a line whose
/// first word (after attributes such as `@[default_target]`) is one of `DISCARDED` followed
/// by a name, once comments and string literals are removed.
fn discarded_command(text: &str) -> Option<&'static str> {
    let code = strip_comments_and_strings(text);
    for line in code.lines() {
        let mut rest = line.trim_start();
        while let Some(r) = rest.strip_prefix("@[") {
            rest = match r.find(']') {
                Some(k) => r[k + 1..].trim_start(),
                None => "",
            };
        }
        let word: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        // a command names what it declares: `script hi do`, `target t pkg : ...`
        let after = &rest[word.len()..];
        let names = after.starts_with(char::is_whitespace)
            && after
                .trim_start()
                .starts_with(|c: char| c.is_alphabetic() || c == '_' || c == '«');
        if let Some(cmd) = DISCARDED.iter().find(|d| **d == word).filter(|_| names) {
            return Some(cmd);
        }
    }
    None
}

/// `text` with Lean comments (`--` and nested `/- -/`) and string literals (`"..."` with
/// escapes, `r"..."`, `r#"..."#`) replaced by spaces; line breaks are kept.
fn strip_comments_and_strings(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let blank = |ch: char| if ch == '\n' { '\n' } else { ' ' };
    let mut i = 0;
    while i < c.len() {
        if c[i] == '-' && c.get(i + 1) == Some(&'-') {
            while i < c.len() && c[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c[i] == '/' && c.get(i + 1) == Some(&'-') {
            let mut depth = 0usize;
            while i < c.len() {
                if c[i] == '/' && c.get(i + 1) == Some(&'-') {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if c[i] == '-' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    out.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(blank(c[i]));
                    i += 1;
                }
            }
        } else if c[i] == 'r'
            && (c.get(i + 1) == Some(&'"') || c.get(i + 1) == Some(&'#'))
            && (i == 0 || !(c[i - 1].is_alphanumeric() || c[i - 1] == '_'))
        {
            let mut j = i + 1;
            let mut hashes = 0;
            while c.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if c.get(j) != Some(&'"') {
                out.push(c[i]);
                i += 1;
                continue;
            }
            j += 1;
            loop {
                if j >= c.len() {
                    break;
                }
                if c[j] == '"' && (0..hashes).all(|k| c.get(j + 1 + k) == Some(&'#')) {
                    j += 1 + hashes;
                    break;
                }
                j += 1;
            }
            for &ch in &c[i..j.min(c.len())] {
                out.push(blank(ch));
            }
            i = j;
        } else if c[i] == '"' {
            out.push(' ');
            i += 1;
            while i < c.len() && c[i] != '"' {
                if c[i] == '\\' && i + 1 < c.len() {
                    out.push(blank(c[i]));
                    i += 1;
                }
                out.push(blank(c[i]));
                i += 1;
            }
            out.push(' ');
            i += 1;
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_discarded_commands() {
        let ok = "import Lake\nopen Lake DSL\n\npackage p where\n  leanOptions := #[]\n\n\
                  @[default_target]\nlean_lib P where\n  globs := #[.submodules `P]\n";
        assert_eq!(discarded_command(ok), None);
        assert_eq!(
            discarded_command(&format!("{ok}\nscript hi do\n  return 0\n")),
            Some("script")
        );
        assert_eq!(
            discarded_command(&format!(
                "{ok}\n@[default_target]\ntarget t pkg : Unit := pure (.nil)\n"
            )),
            Some("target")
        );
        assert_eq!(
            discarded_command(&format!(
                "{ok}\nmeta if true then\n  extern_lib foo pkg := pure .nil\n"
            )),
            Some("extern_lib")
        );
        assert_eq!(
            discarded_command(&format!("{ok}\nmodule_facet f mod : Unit := pure .nil\n")),
            Some("module_facet")
        );
    }

    #[test]
    fn ignores_comments_strings_and_identifiers() {
        let text = "-- script in a comment\n/- nested /- target -/\nscript -/\n\
                    def s := \"\nscript x\n\"\ndef r := r#\"\ntarget \"# \"x\"\n\
                    def targets := 1\nlean_lib script_lib where\n  target := 1\n  target.foo\n";
        assert_eq!(discarded_command(text), None);
        assert_eq!(
            strip_comments_and_strings("a -- b\nc")
                .lines()
                .collect::<Vec<_>>(),
            vec!["a     ", "c"]
        );
    }
}
