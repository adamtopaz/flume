//! Finding the Lean toolchain a project builds with, without starting a process.
//!
//! Resolution follows elan's rules where they are unambiguous: `FLUME_LEAN_SYSROOT`
//! (an explicit override), else `ELAN_TOOLCHAIN`, else the nearest `lean-toolchain`
//! file above the project. A toolchain name is tried as a path relative to that file,
//! then as `<origin>:<release>` installed under `$ELAN_HOME/toolchains`, then as a
//! linked toolchain of that name. Anything else falls back to `lean --print-prefix`.
//! The githash is read from the header of the toolchain's `Init.olean` (bytes 40..80),
//! which equals `lean --githash` for any working installation.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug)]
pub struct Toolchain {
    pub sysroot: PathBuf,
    pub lean: PathBuf,
    pub githash: String,
    pub version: String,
}

fn elan_home() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("ELAN_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(h));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".elan"))
}

fn toolchain_name(project: &Path) -> Option<(String, PathBuf)> {
    if let Some(t) = std::env::var("ELAN_TOOLCHAIN").ok().filter(|v| !v.trim().is_empty()) {
        return Some((t.trim().to_string(), project.to_path_buf()));
    }
    let mut dir = project.canonicalize().ok()?;
    loop {
        let f = dir.join("lean-toolchain");
        if let Ok(text) = std::fs::read_to_string(&f) {
            let name = text.lines().next().unwrap_or("").trim().to_string();
            if !name.is_empty() {
                return Some((name, dir));
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn from_elan(project: &Path) -> Option<PathBuf> {
    let (name, base) = toolchain_name(project)?;
    let as_path = base.join(&name);
    if as_path.join("bin").join("lean").is_file() {
        return as_path.canonicalize().ok();
    }
    let home = elan_home()?;
    let dir = match name.split_once(':') {
        Some((origin, release)) => home.join("toolchains").join(format!("{}---{}", origin.replace('/', "--"), release)),
        None => home.join("toolchains").join(&name),
    };
    if dir.join("bin").join("lean").is_file() {
        dir.canonicalize().ok()
    } else {
        None
    }
}

fn print_prefix(project: &Path) -> Result<PathBuf, String> {
    let out = Command::new("lean")
        .arg("--print-prefix")
        .current_dir(project)
        .output()
        .map_err(|e| format!("cannot run `lean --print-prefix` to find the Lean toolchain: {e}"))?;
    if !out.status.success() {
        return Err(format!("`lean --print-prefix` failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// githash and version string from an `.olean` header: `olean`, a format byte, a flags
/// byte, the version padded to 33 bytes, then the 40-byte githash.
fn olean_identity(init_olean: &Path) -> Option<(String, String)> {
    use std::io::Read;
    let mut buf = [0u8; 80];
    std::fs::File::open(init_olean).ok()?.read_exact(&mut buf).ok()?;
    if &buf[0..5] != b"olean" {
        return None;
    }
    let version = String::from_utf8_lossy(&buf[7..40]).trim_end_matches('\0').to_string();
    let githash = std::str::from_utf8(&buf[40..80]).ok()?.to_string();
    if githash.len() == 40 && githash.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some((githash, version))
    } else {
        None
    }
}

pub fn detect(project: &Path) -> Result<Toolchain, String> {
    let sysroot = match std::env::var_os("FLUME_LEAN_SYSROOT").filter(|v| !v.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => match from_elan(project) {
            Some(p) => p,
            None => print_prefix(project)?,
        },
    };
    let lean = sysroot.join("bin").join("lean");
    if !lean.is_file() {
        return Err(format!("no Lean executable at {}", lean.display()));
    }
    let (githash, version) = match olean_identity(&sysroot.join("lib").join("lean").join("Init.olean")) {
        Some(id) => id,
        None => {
            let out = Command::new(&lean).arg("--githash").output().map_err(|e| format!("cannot run {}: {e}", lean.display()))?;
            (String::from_utf8_lossy(&out.stdout).trim().to_string(), String::new())
        }
    };
    Ok(Toolchain { sysroot, lean, githash, version })
}
