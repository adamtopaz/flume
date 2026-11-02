//! What flume remembers between builds (`.lake/flume/state.json`).
//!
//! For each module: the file status and content hash of its source and its parsed
//! imports (so an unchanged source is neither read nor parsed again), the key it was
//! last built with, and the content hashes of its outputs. A module is up to date when
//! its key (a hash of everything that determines its outputs) matches and its outputs
//! still exist.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub const STATE_VERSION: u32 = 1;

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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Built {
    pub key: String,
    /// facet -> content hash
    pub outputs: Vec<(String, String)>,
    /// status of the `.olean`, to notice deleted or replaced outputs cheaply
    pub olean_stat: FileStat,
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
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(tmp, path)
    }
}
