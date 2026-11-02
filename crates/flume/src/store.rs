//! What flume remembers between builds (`.lake/flume/state.json`).
//!
//! For each module: the file status and content hash of its source and its parsed
//! imports (so an unchanged source is neither read nor parsed again), the key it was
//! last built with, and the content hashes and file status of its outputs. A module is
//! up to date when its key (a hash of everything that determines its outputs) matches
//! and every output still exists with the remembered status or, failing that, content.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

pub const STATE_VERSION: u32 = 2;

/// A source modified this recently is not trusted by status alone next time ("racy"
/// timestamps: an edit within the clock's granularity after the read would be missed).
const RACY_NS: i128 = 2_000_000_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileStat {
    pub size: u64,
    pub mtime_ns: i128,
    pub ctime_ns: i128,
    pub ino: u64,
}

impl FileStat {
    pub fn of(path: &Path) -> std::io::Result<FileStat> {
        let md = std::fs::metadata(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(FileStat {
                size: md.size(),
                mtime_ns: md.mtime() as i128 * 1_000_000_000 + md.mtime_nsec() as i128,
                ctime_ns: md.ctime() as i128 * 1_000_000_000 + md.ctime_nsec() as i128,
                ino: md.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            let mtime = md
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as i128)
                .unwrap_or(0);
            Ok(FileStat {
                size: md.len(),
                mtime_ns: mtime,
                ctime_ns: 0,
                ino: 0,
            })
        }
    }

    /// Whether the file changed too recently for its status to be trusted later.
    pub fn is_racy(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i128)
            .unwrap_or(0);
        now - self.mtime_ns.max(self.ctime_ns) < RACY_NS
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredImport {
    pub module: Vec<String>,
    pub all: bool,
    pub exported: bool,
    pub meta: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceInfo {
    pub stat: FileStat,
    pub hash: String,
    pub is_module: bool,
    pub imports: Vec<StoredImport>,
    /// the status was taken too close to a modification to be trusted next time
    #[serde(default)]
    pub racy: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Built {
    pub key: String,
    /// facet -> content hash
    pub outputs: Vec<(String, String)>,
    /// facet -> file status, to notice deleted or replaced outputs cheaply
    pub stats: Vec<(String, FileStat)>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub githash: String,
    pub sources: HashMap<String, SourceInfo>,
    pub built: HashMap<String, Built>,
}

impl State {
    pub fn load(path: &Path, githash: &str) -> State {
        let fresh = State {
            version: STATE_VERSION,
            githash: githash.to_string(),
            ..Default::default()
        };
        let Ok(bytes) = std::fs::read(path) else {
            return fresh;
        };
        match serde_json::from_slice::<State>(&bytes) {
            Ok(s) if s.version == STATE_VERSION && s.githash == githash => s,
            _ => fresh,
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(tmp, path)
    }
}

/// An exclusive lock on a checkout's flume state, held for the duration of a build so
/// that two builds of one checkout do not interleave.
pub struct BuildLock {
    _file: File,
}

impl BuildLock {
    pub fn acquire(path: &Path) -> std::io::Result<BuildLock> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            const LOCK_EX: i32 = 2;
            // SAFETY: flock on a file descriptor we own; it is released when `file` closes.
            if unsafe { flock(file.as_raw_fd(), LOCK_EX) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(BuildLock { _file: file })
    }
}
