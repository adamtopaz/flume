//! flume: a minimal, fast build system for Lean 4.
//!
//! Commands:
//!   flume build [TARGET...]     build library or module targets (default: the package's)
//!   flume query MOD:FACET...    print the path of each module artifact (does not build)
//!   flume clean                 remove the build directory and flume's state
//!   flume --version
//!
//! Options (before the command): `-d DIR`/`--dir DIR` runs in DIR; `-j N` sets the
//! number of concurrent compilations (default: `LEAN_NUM_THREADS`, else the number of
//! CPUs available); `-v` prints every message.
//!
//! Environment: `FLUME_CACHE_DIR` sets the artifact cache shared by checkouts (default
//! `$XDG_CACHE_HOME/flume`, else `~/.cache/flume`); `FLUME_CACHE=off` disables it.

mod build;
mod cache;
mod config;
mod header;
mod store;
mod toolchain;

use std::path::PathBuf;

const USAGE: &str = "usage: flume [-d DIR] [-j N] [-v] <build [TARGET...] | query MOD:FACET... | clean | --version>";

fn default_jobs() -> usize {
    std::env::var("LEAN_NUM_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        })
}

fn run() -> Result<i32, String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut dir = PathBuf::from(".");
    let mut jobs = default_jobs();
    let mut verbose = false;
    loop {
        match args.first().map(String::as_str) {
            Some("-d") | Some("--dir") => {
                if args.len() < 2 {
                    return Err(USAGE.into());
                }
                dir = PathBuf::from(&args[1]);
                args.drain(..2);
            }
            Some("-j") => {
                if args.len() < 2 {
                    return Err(USAGE.into());
                }
                jobs = args[1]
                    .parse()
                    .map_err(|_| format!("-j expects a number, got {}", args[1]))?;
                args.drain(..2);
            }
            Some("-v") | Some("--verbose") => {
                verbose = true;
                args.remove(0);
            }
            _ => break,
        }
    }
    let cmd = if args.is_empty() {
        "build".to_string()
    } else {
        args.remove(0)
    };
    let root = dir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    match cmd.as_str() {
        "--version" | "version" => {
            let tc = toolchain::detect(&root).ok();
            match tc {
                Some(tc) => println!(
                    "flume {} (Lean {}, commit {})",
                    env!("CARGO_PKG_VERSION"),
                    tc.version,
                    tc.githash
                ),
                None => println!("flume {}", env!("CARGO_PKG_VERSION")),
            }
            Ok(0)
        }
        "build" => {
            let pkg = config::load(&root)?;
            let tc = toolchain::detect(&root)?;
            let ok = build::build(
                &pkg,
                &tc,
                &build::Options {
                    targets: args,
                    jobs,
                    verbose,
                },
            )?;
            Ok(if ok { 0 } else { 1 })
        }
        "query" => {
            let pkg = config::load(&root)?;
            for t in &args {
                let (m, facet) = t
                    .rsplit_once(':')
                    .ok_or_else(|| format!("expected MOD:FACET, got {t}"))?;
                println!(
                    "{}",
                    build::artifact_path(&pkg, &config::split_name(m), facet).display()
                );
            }
            Ok(0)
        }
        "clean" => {
            let pkg = config::load(&root)?;
            for p in [pkg.build_dir.clone(), root.join(".lake").join("flume")] {
                if p.exists() {
                    std::fs::remove_dir_all(&p)
                        .map_err(|e| format!("cannot remove {}: {e}", p.display()))?;
                }
            }
            Ok(0)
        }
        "debug-headers" => {
            // flume's parse of each file's header, in flume-worker's JSON format
            for f in &args {
                let line = match std::fs::read(f) {
                    Err(e) => serde_json::json!({"file": f, "error": e.to_string()}),
                    Ok(bytes) => match header::parse(&String::from_utf8_lossy(&bytes)) {
                        Err(e) => serde_json::json!({"file": f, "error": e.msg}),
                        Ok(h) => serde_json::json!({
                            "file": f,
                            "isModule": h.is_module,
                            "imports": h.imports.iter().map(|i| serde_json::json!({
                                "module": header::name_to_string(&i.module),
                                "importAll": i.all, "isExported": i.exported, "isMeta": i.meta,
                            })).collect::<Vec<_>>(),
                        }),
                    },
                };
                println!("{line}");
            }
            Ok(0)
        }
        "server" => {
            // No build server yet (roadmap); the benchmark asks with `server protocol`.
            eprintln!("flume has no build server yet");
            Ok(1)
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(0)
        }
        other => Err(format!("unknown command `{other}`\n{USAGE}")),
    }
}

fn main() {
    let code = match run() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    };
    std::process::exit(code);
}
