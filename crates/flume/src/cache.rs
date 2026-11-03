//! The artifact cache shared by every checkout on the machine (roadmap P2).
//!
//! Modeled on Lake's own (`Cache.saveArtifact` and `restoreArtifact` in
//! `Lake/Build/Common.lean`): artifacts are stored once by content, and a module's key
//! (which hashes everything Lake traces, and no paths) maps to the content hashes of the
//! outputs it was built with. A checkout whose module key is in the cache gets that
//! module's outputs hard-linked into its build directory, exactly where a compile would
//! have put them, instead of compiling.
//!
//! Layout under the root (`$FLUME_CACHE_DIR`, else `$XDG_CACHE_HOME/flume`, else
//! `~/.cache/flume`, each followed by `v1`; `FLUME_CACHE=off` disables the cache):
//! - `art/<h[..2]>/<h>`: an artifact whose blake3 content hash is `h`, read-only;
//! - `key/<os>-<arch>/<k[..2]>/<k>.json`: the outputs (facet, content hash, size) and
//!   the messages of a successful compile with key `k`.
//!
//! Every file appears atomically (hard link, or a rename of a finished temporary file),
//! so concurrent builds may share the cache; an entry is written only after its
//! artifacts are in place. A cache that cannot be read or written never fails a build:
//! the module is compiled instead.

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const LAYOUT: &str = "v1";

/// One output of a cached compile.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedOutput {
    pub facet: String,
    pub hash: String,
    pub size: u64,
}

/// What a key maps to: the outputs of the compile and the messages it logged.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub outputs: Vec<CachedOutput>,
    pub messages: Vec<String>,
}

pub struct Cache {
    root: PathBuf,
}

/// The cache root for these environment values, or `None` when the cache is off.
fn resolve(
    flume_cache: Option<OsString>,
    flume_cache_dir: Option<OsString>,
    xdg_cache_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if flume_cache.is_some_and(|v| matches!(v.to_str(), Some("off" | "0" | "false"))) {
        return None;
    }
    let nonempty = |v: Option<OsString>| v.filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if let Some(d) = nonempty(flume_cache_dir) {
        d
    } else if let Some(x) = nonempty(xdg_cache_home).filter(|p| p.is_absolute()) {
        x.join("flume") // the XDG spec ignores a relative XDG_CACHE_HOME
    } else {
        nonempty(home)?.join(".cache").join("flume")
    };
    Some(base.join(LAYOUT))
}

fn tmp_name(dir: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(unix)]
fn set_read_only(p: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o444))
}

#[cfg(not(unix))]
fn set_read_only(p: &Path) -> io::Result<()> {
    let mut perms = std::fs::metadata(p)?.permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(p, perms)
}

/// Copy `src` to `dst` through a temporary file in `dst`'s directory, so `dst` appears
/// complete or not at all.
fn copy_atomic(src: &Path, dst: &Path) -> io::Result<()> {
    let dir = dst.parent().unwrap_or(Path::new("."));
    let tmp = tmp_name(dir);
    let r = std::fs::copy(src, &tmp)
        .and_then(|_| set_read_only(&tmp))
        .and_then(|_| std::fs::rename(&tmp, dst));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// Hard-link `src` to `dst` (which must not exist), creating `dst`'s directory if needed
/// and copying if the file system refuses the link.
fn link_or_copy(src: &Path, dst: &Path) -> io::Result<()> {
    let mut r = std::fs::hard_link(src, dst);
    if matches!(&r, Err(e) if e.kind() == io::ErrorKind::NotFound) && src.exists() {
        if let Some(d) = dst.parent() {
            std::fs::create_dir_all(d)?;
        }
        r = std::fs::hard_link(src, dst);
    }
    match r {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::AlreadyExists
            ) =>
        {
            Err(e)
        }
        Err(_) => copy_atomic(src, dst), // another file system, too many links, ...
    }
}

/// Put a hard link to (or else a copy of) `src` at `dst`, atomically replacing whatever
/// is there.
fn install(src: &Path, dst: &Path) -> io::Result<()> {
    let dir = dst.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = tmp_name(dir);
    if std::fs::hard_link(src, &tmp).is_err() {
        return copy_atomic(src, dst);
    }
    let r = std::fs::rename(&tmp, dst);
    let _ = std::fs::remove_file(&tmp); // rename leaves it if both already were one file
    r
}

impl Cache {
    /// The cache this process's environment selects, if any.
    pub fn from_env() -> Option<Cache> {
        resolve(
            std::env::var_os("FLUME_CACHE"),
            std::env::var_os("FLUME_CACHE_DIR"),
            std::env::var_os("XDG_CACHE_HOME"),
            std::env::var_os("HOME"),
        )
        .map(|root| Cache { root })
    }

    #[cfg(test)]
    pub fn at(root: PathBuf) -> Cache {
        Cache { root }
    }

    /// A directory in the cache for other things flume keeps per machine.
    pub fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn art_path(&self, hash: &str) -> PathBuf {
        self.root
            .join("art")
            .join(&hash[..2.min(hash.len())])
            .join(hash)
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        self.root
            .join("key")
            .join(platform)
            .join(&key[..2.min(key.len())])
            .join(format!("{key}.json"))
    }

    /// The entry stored for `key`, if there is a readable one.
    pub fn lookup(&self, key: &str) -> Option<Entry> {
        let bytes = std::fs::read(self.entry_path(key)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Store a successful compile: each output (facet, path, content hash) is made
    /// read-only, as Lake does, so that writing to a hard-linked copy cannot corrupt the
    /// cache, and linked into `art/` unless an intact copy is there; then the entry for
    /// `key` is written.
    pub fn store(
        &self,
        key: &str,
        outputs: &[(String, PathBuf, String)],
        messages: &[String],
    ) -> io::Result<()> {
        let mut cached = Vec::with_capacity(outputs.len());
        for (facet, path, hash) in outputs {
            set_read_only(path)?;
            let size = std::fs::metadata(path)?.len();
            let art = self.art_path(hash);
            // a cached file of another size is damaged: replace it
            if std::fs::metadata(&art).map(|m| m.len()).ok() != Some(size) {
                install(path, &art)?;
            }
            cached.push(CachedOutput {
                facet: facet.clone(),
                hash: hash.clone(),
                size,
            });
        }
        let entry = Entry {
            outputs: cached,
            messages: messages.to_vec(),
        };
        let dst = self.entry_path(key);
        let dir = dst.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = tmp_name(dir);
        let r = std::fs::write(&tmp, serde_json::to_vec(&entry)?)
            .and_then(|_| std::fs::rename(&tmp, &dst));
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        r
    }

    /// Put each output of `entry` at its path (`dest(facet)`), which must not exist.
    /// Fails if an artifact is missing from the cache or has the wrong size; the caller
    /// then compiles, which removes whatever was restored.
    pub fn restore(&self, entry: &Entry, dest: impl Fn(&str) -> PathBuf) -> io::Result<()> {
        for o in &entry.outputs {
            let dst = dest(&o.facet);
            link_or_copy(&self.art_path(&o.hash), &dst)?;
            let size = std::fs::metadata(&dst)?.len();
            if size != o.size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("cached {} has {size} bytes, not {}", o.facet, o.size),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("flume-cache-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn hash(bytes: &[u8]) -> String {
        blake3::hash(bytes).to_hex().to_string()
    }

    #[test]
    fn root_follows_the_environment() {
        let s = |v: &str| Some(OsString::from(v));
        assert_eq!(
            resolve(None, s("/c"), s("/x"), s("/h")),
            Some(PathBuf::from("/c/v1"))
        );
        assert_eq!(
            resolve(None, None, s("/x"), s("/h")),
            Some(PathBuf::from("/x/flume/v1"))
        );
        assert_eq!(
            resolve(None, s(""), s("rel"), s("/h")),
            Some(PathBuf::from("/h/.cache/flume/v1"))
        );
        assert_eq!(resolve(s("off"), s("/c"), s("/x"), s("/h")), None);
        assert_eq!(resolve(None, None, None, None), None);
    }

    #[test]
    fn store_then_restore_elsewhere() {
        let d = scratch("roundtrip");
        let cache = Cache::at(d.join("cache"));
        let a = d.join("a");
        std::fs::create_dir_all(&a).unwrap();
        let (olean, c) = (a.join("M.olean"), a.join("M.c"));
        std::fs::write(&olean, b"olean bytes").unwrap();
        std::fs::write(&c, b"c bytes").unwrap();
        let outs = vec![
            ("olean".to_string(), olean.clone(), hash(b"olean bytes")),
            ("c".to_string(), c.clone(), hash(b"c bytes")),
        ];
        let msgs = vec!["warning: M.lean:1:0: unused".to_string()];
        cache.store("k1", &outs, &msgs).unwrap();
        assert!(std::fs::metadata(&olean).unwrap().permissions().readonly());
        // storing the same content again (another checkout) is fine
        cache.store("k1", &outs, &msgs).unwrap();

        let entry = cache.lookup("k1").unwrap();
        assert_eq!(entry.messages, msgs);
        assert_eq!(entry.outputs[0].size, 11);
        assert!(cache.lookup("k2").is_none());

        let b = d.join("b").join("deep").join("dir");
        cache.restore(&entry, |f| b.join(format!("M.{f}"))).unwrap();
        assert_eq!(std::fs::read(b.join("M.olean")).unwrap(), b"olean bytes");
        assert_eq!(std::fs::read(b.join("M.c")).unwrap(), b"c bytes");
        assert!(std::fs::metadata(b.join("M.c"))
            .unwrap()
            .permissions()
            .readonly());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_or_damaged_artifacts_fail_the_restore() {
        let d = scratch("damaged");
        let cache = Cache::at(d.join("cache"));
        let f = d.join("M.olean");
        std::fs::write(&f, b"0123456789").unwrap();
        let h = hash(b"0123456789");
        cache
            .store("k", &[("olean".to_string(), f.clone(), h.clone())], &[])
            .unwrap();
        let entry = cache.lookup("k").unwrap();

        // wrong size (e.g. a damaged cache file)
        let mut wrong = entry.clone();
        wrong.outputs[0].size = 3;
        let out = d.join("out1");
        assert!(cache.restore(&wrong, |_| out.join("M.olean")).is_err());

        // evicted artifact
        std::fs::remove_file(cache.art_path(&h)).unwrap();
        let out = d.join("out2");
        assert!(cache.restore(&entry, |_| out.join("M.olean")).is_err());
        assert!(!out.join("M.olean").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unreadable_entries_are_misses() {
        let d = scratch("garbage");
        let cache = Cache::at(d.join("cache"));
        let p = cache.entry_path("abc");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"{not json").unwrap();
        assert!(cache.lookup("abc").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
