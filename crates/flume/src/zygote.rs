//! A fork server for `lean` (roadmap P1).
//!
//! `zygote.c` initializes Lean once, exactly as `lean` does, and then forks one child per
//! compile request; each child runs `lean`'s own driver (`lean_shell_main`) on the request's
//! arguments, so it writes the same artifacts as `lean` without paying for `exec`, dynamic
//! linking, and Lean's initialization (~20 ms) on every module.
//!
//! flume embeds the C source and compiles it on first use with the system's `cc` against
//! the toolchain's `libleanshared`, into the artifact cache (or `.lake/flume` without one),
//! keyed by the source and the toolchain. The zygote starts in the background at a build's
//! first compile and takes requests once it has printed `ready`; until then, and whenever it
//! is unavailable (no C compiler, another Lean release than the one the C mirrors, a crash),
//! compiles run `lean` directly (also with `FLUME_ZYGOTE=off`). At the end of the build
//! every request has been answered, so the zygote has no children; it is killed and reaped,
//! and nothing outlives the build.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

const SOURCE: &str = include_str!("zygote.c");
/// The Lean release whose `lean_main` (`src/util/shell.cpp`) `zygote.c` mirrors.
const LEAN_VERSION: &str = "4.35.0";

/// How to build and run the zygote for one build.
struct Config {
    /// the toolchain's installation prefix (`bin/lean`, `include/`, `lib/lean/`)
    prefix: PathBuf,
    /// where the compiled zygote lives (shared by builds through the cache)
    exe: PathBuf,
    /// the package root: the zygote's (and so every child's) working directory
    cwd: PathBuf,
    /// `LEAN_PATH` for the children: `lean`'s, plus the toolchain's `lib/lean`, which
    /// `lean` finds relative to its own location and the zygote cannot
    lean_path: OsString,
    /// where children write their output
    io_dir: PathBuf,
}

#[derive(Default)]
struct State {
    /// the zygote, once spawned (to kill it at the end)
    child: Option<Child>,
    /// its input, once it has printed `ready`
    stdin: Option<ChildStdin>,
    /// requests waiting for their exit code
    pending: HashMap<u64, Sender<i32>>,
}

pub struct Zygote {
    config: Option<Config>,
    state: Arc<Mutex<State>>,
    started: AtomicBool,
    stopping: Arc<AtomicBool>,
    next_id: AtomicU64,
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// What a compile through the zygote produced: exit code, stdout, stderr.
pub type Output = (i32, Vec<u8>, Vec<u8>);

impl Zygote {
    /// A zygote for `lean` at `lean_exe` (release `version`) building in `cwd` with
    /// `LEAN_PATH=lib_dir`; `bin_root` is where compiled zygotes are kept. Disabled when
    /// the release differs from the one the C mirrors or the toolchain lacks the headers.
    pub fn new(
        lean_exe: &Path,
        version: &str,
        cwd: &Path,
        lib_dir: &Path,
        bin_root: PathBuf,
    ) -> Zygote {
        let off = std::env::var_os("FLUME_ZYGOTE")
            .is_some_and(|v| matches!(v.to_str(), Some("off" | "0" | "false")));
        let config = (cfg!(unix) && !off && version == LEAN_VERSION)
            .then(|| lean_exe.parent()?.parent().map(Path::to_path_buf))
            .flatten()
            .filter(|p| p.join("include/lean/lean.h").is_file())
            .and_then(|prefix| {
                let key =
                    blake3::hash(format!("{SOURCE}\0{}\0{version}", prefix.display()).as_bytes())
                        .to_hex();
                let lean_path =
                    std::env::join_paths([lib_dir.to_path_buf(), prefix.join("lib/lean")]).ok()?;
                Some(Config {
                    exe: bin_root.join(&key[..16]).join("flume-zygote"),
                    prefix,
                    cwd: cwd.to_path_buf(),
                    lean_path,
                    io_dir: cwd.join(".lake").join("flume").join("zygote"),
                })
            });
        Zygote {
            config,
            state: Arc::new(Mutex::new(State::default())),
            started: AtomicBool::new(false),
            stopping: Arc::new(AtomicBool::new(false)),
            next_id: AtomicU64::new(0),
            thread: Mutex::new(None),
        }
    }

    /// Run `lean` with `args` (without `argv[0]`) in a child of the zygote. `None` means
    /// the zygote cannot take it (not ready yet, unavailable, or it failed): the caller
    /// runs `lean` itself.
    pub fn run(&self, lean_exe: &Path, args: &[OsString]) -> Option<Output> {
        let cfg = self.config.as_ref()?;
        if !self.started.swap(true, Ordering::SeqCst) {
            self.start(cfg);
            return None;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let out = cfg.io_dir.join(format!("{id}.out"));
        let err = cfg.io_dir.join(format!("{id}.err"));
        let mut fields: Vec<Vec<u8>> = vec![
            id.to_string().into_bytes(),
            path_bytes(&out),
            path_bytes(&err),
            os_bytes(&cfg.lean_path),
            path_bytes(lean_exe),
        ];
        fields.extend(args.iter().map(|a| os_bytes(a)));
        if fields.iter().any(|f| f.contains(&0)) {
            return None;
        }
        let mut req = format!("{}\0", fields.len()).into_bytes();
        for f in &fields {
            req.extend_from_slice(f);
            req.push(0);
        }
        let (tx, rx) = channel();
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let stdin = st.stdin.as_mut()?;
            if stdin.write_all(&req).and_then(|_| stdin.flush()).is_err() {
                st.stdin = None;
                return None;
            }
            st.pending.insert(id, tx);
        }
        // an error means the zygote died, and its children with it (PR_SET_PDEATHSIG)
        let code = rx.recv().ok();
        let result = code.map(|c| {
            (
                c,
                std::fs::read(&out).unwrap_or_default(),
                std::fs::read(&err).unwrap_or_default(),
            )
        });
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(&err);
        result
    }

    /// Build the zygote if needed and spawn it, in the background; the same thread then
    /// reads its replies.
    fn start(&self, cfg: &Config) {
        let (exe, prefix, cwd, io_dir) = (
            cfg.exe.clone(),
            cfg.prefix.clone(),
            cfg.cwd.clone(),
            cfg.io_dir.clone(),
        );
        let state = Arc::clone(&self.state);
        let stopping = Arc::clone(&self.stopping);
        let handle = std::thread::spawn(move || {
            if !exe.is_file() && !compile(&exe, &prefix) {
                return;
            }
            if std::fs::create_dir_all(&io_dir).is_err() {
                return;
            }
            let (stdout, stdin) = {
                // under the lock, so that `finish` either sees the child or stops its spawn
                let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                if stopping.load(Ordering::SeqCst) {
                    return;
                }
                let spawned = Command::new(&exe)
                    .current_dir(&cwd)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn();
                let Ok(mut child) = spawned else { return };
                let pipes = (child.stdout.take(), child.stdin.take());
                st.child = Some(child);
                match pipes {
                    (Some(o), Some(i)) => (o, i),
                    _ => return,
                }
            };
            // requests may be written only after `ready`
            let mut lines = BufReader::new(stdout).lines();
            match lines.next() {
                Some(Ok(l)) if l == "ready" => {
                    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                    if !stopping.load(Ordering::SeqCst) {
                        st.stdin = Some(stdin);
                    }
                }
                _ => return,
            }
            for line in lines {
                let Ok(line) = line else { break };
                let mut it = line.split(' ');
                let (Some(id), Some(code)) = (it.next(), it.next()) else {
                    continue;
                };
                let (Ok(id), Ok(code)) = (id.parse::<u64>(), code.parse::<i32>()) else {
                    continue;
                };
                let tx = state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pending
                    .remove(&id);
                if let Some(tx) = tx {
                    let _ = tx.send(code);
                }
            }
            // the zygote is gone: fail whatever is still waiting
            let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
            st.stdin = None;
            st.pending.clear();
        });
        *self.thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    /// End of the build: stop the zygote (every request has been answered, so it has no
    /// children) and wait for it and for the thread that started it.
    pub fn finish(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let child = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.stdin = None;
            st.child.take()
        };
        if let Some(mut c) = child {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(h) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
        // a zygote spawned while we were stopping it (the lock orders this, but be sure)
        let child = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .child
            .take();
        if let Some(mut c) = child {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Compile the zygote to `exe` (atomically); on failure leave a marker so that later
/// builds do not try again.
fn compile(exe: &Path, prefix: &Path) -> bool {
    let Some(dir) = exe.parent() else {
        return false;
    };
    let failed = dir.join("failed");
    if failed.exists() || std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let src = dir.join(format!("zygote-{}.c", std::process::id()));
    let tmp = dir.join(format!(".flume-zygote-{}", std::process::id()));
    let lib = prefix.join("lib/lean");
    let ok = std::fs::write(&src, SOURCE).is_ok()
        && Command::new("cc")
            .arg("-O2")
            .arg("-o")
            .arg(&tmp)
            .arg(&src)
            .arg("-I")
            .arg(prefix.join("include"))
            .arg("-L")
            .arg(&lib)
            .arg("-lleanshared")
            .arg(format!("-Wl,-rpath,{}", lib.display()))
            .arg("-lpthread")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        && std::fs::rename(&tmp, exe).is_ok();
    let _ = std::fs::remove_file(&src);
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::write(&failed, b"");
    }
    ok
}

#[cfg(unix)]
fn os_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    s.to_string_lossy().as_bytes().to_vec()
}

fn path_bytes(p: &Path) -> Vec<u8> {
    os_bytes(p.as_os_str())
}
