use anyhow::{Context, Result};
use directories::BaseDirs;
use fs4::FileExt;
use std::env;
use std::fs;
use std::collections::BTreeSet;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use zip::ZipArchive;

// These will be replaced during the build process with actual embedded data
// The build script will generate a data.rs file with the actual data
include!(concat!(env!("OUT_DIR"), "/data.rs"));

/// Name of the marker file written after a successful extraction. Its contents are the
/// resolved main script path, so the warm path never has to parse package.json.
const READY_FILE: &str = ".ready";

/// Entries at or below this size are decompressed into a reusable buffer and written with a
/// single `write_all`; larger ones stream through a buffered writer.
const SMALL_ENTRY_BYTES: u64 = 8 * 1024 * 1024;

/// One archive entry's destination, resolved during the planning pass so the parallel write
/// pass never has to touch shared metadata.
struct PlannedFile {
    index: usize,
    path: PathBuf,
    size: u64,
    #[cfg(unix)]
    mode: Option<u32>,
}

fn main() -> Result<()> {
    // args_os, not args: a bundled app must be able to receive non-UTF-8 arguments such as
    // a filename in an arbitrary encoding. `env::args()` panics on those.
    let args: Vec<std::ffi::OsString> = env::args_os().collect();

    let cache_dir = get_cache_dir_fast().context("Failed to determine cache directory")?;
    let app_dir = cache_dir.join(BUILD_ID);

    // Warm path: a single read of the ready marker gives us everything we need. If anything
    // about the cache is stale or damaged, `run_app` returns and we fall through to a full
    // re-extraction below, so we do not need to stat the payload up front.
    if let Ok(main_script) = fs::read_to_string(app_dir.join(READY_FILE)) {
        let main_script = main_script.trim();
        if !main_script.is_empty() {
            run_app(&app_dir, main_script, &args[1..])?;
        }
    }

    // Cold path: extract under an exclusive lock so concurrent launches cooperate.
    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;
    let lock_file_path = cache_dir.join(format!("{BUILD_ID}.lock"));
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_file_path)
        .with_context(|| format!("Failed to create lock file at {}", lock_file_path.display()))?;

    FileExt::lock(&lock_file).context("Failed to acquire extraction lock")?;

    // Another process may have completed the extraction while we waited for the lock. Keep
    // holding the lock across this attempt: a successful exec drops it automatically (the fd
    // is close-on-exec), and if the cache turns out to be unusable we still own the right to
    // re-extract below.
    let ready_path = app_dir.join(READY_FILE);
    if let Ok(main_script) = fs::read_to_string(&ready_path) {
        let main_script = main_script.trim().to_string();
        if !main_script.is_empty() {
            run_app(&app_dir, &main_script, &args[1..])?;
        }
    }

    extract_application(&app_dir)
        .with_context(|| format!("Failed to extract application to {}", app_dir.display()))?;

    let main_script = find_main_script(&app_dir.join("app"))?;

    // The ready marker doubles as the cache of the resolved main script. Write it last so a
    // partially extracted directory is never mistaken for a usable one.
    fs::write(&ready_path, &main_script)
        .with_context(|| format!("Failed to create ready file at {}", ready_path.display()))?;

    FileExt::unlock(&lock_file)
        .context("Failed to release extraction lock")?;

    run_app(&app_dir, &main_script, &args[1..])?;

    // `run_app` only returns when it could not start Node at all.
    Err(anyhow::anyhow!(
        "Failed to execute Node.js application from {}",
        app_dir.display()
    ))
}

fn get_cache_dir_fast() -> Result<PathBuf> {
    // Deliberately does not create the directory: the warm path never needs it to exist, and
    // create_dir_all costs a syscall on every launch.
    let base = BaseDirs::new().context("Failed to determine base directories")?;
    Ok(base.cache_dir().join("banderole"))
}

fn get_node_executable_path(app_dir: &Path) -> PathBuf {
    let node_dir = app_dir.join("node");
    if cfg!(windows) {
        node_dir.join("node.exe")
    } else {
        node_dir.join("bin").join("node")
    }
}

fn extract_application(app_dir: &Path) -> Result<()> {
    // Remove existing directory if it exists to ensure clean extraction
    if app_dir.exists() {
        fs::remove_dir_all(app_dir).context("Failed to remove existing app directory")?;
    }

    fs::create_dir_all(app_dir).context("Failed to create app directory")?;

    // Parsed exactly once. `ZipArchive` holds its central directory behind an `Arc`, so the
    // per-worker clones below are O(1) — re-opening the archive per worker instead would
    // re-parse every entry per thread, which is the difference between 110k and 1.7M entry
    // parses on a large project.
    let archive =
        ZipArchive::new(Cursor::new(ZIP_DATA)).context("Failed to open embedded zip archive")?;
    let len = archive.len();

    // Resolve every entry's destination path up front (metadata only), so the write pass can
    // run in parallel without touching the shared central directory.
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut files: Vec<PlannedFile> = Vec::with_capacity(len);
    {
        let mut archive = archive.clone();
        // Entries arrive in directory-walk order, so consecutive files nearly always share a
        // parent. Remembering the last one turns ~100k set insertions into ~10k.
        let mut last_parent: Option<PathBuf> = None;
        for index in 0..len {
            let entry = archive
                .by_index_raw(index)
                .context("Failed to read zip entry")?;
            let is_dir = entry.is_dir() || entry.name().ends_with('/');
            let Some(path) = safe_join(app_dir, entry.name()) else {
                continue;
            };
            let size = entry.size();
            #[cfg(unix)]
            let mode = entry.unix_mode();
            drop(entry);

            if is_dir {
                dirs.insert(path);
                continue;
            }

            if let Some(parent) = path.parent() {
                // Archives do not reliably carry directory entries, so every file's parent is
                // recorded too.
                if last_parent.as_deref() != Some(parent) {
                    dirs.insert(parent.to_path_buf());
                    last_parent = Some(parent.to_path_buf());
                }
            }
            files.push(PlannedFile {
                index,
                path,
                size,
                #[cfg(unix)]
                mode,
            });
        }
    }

    // Sorted order means a parent is always created before its children, and `create_dir_all`
    // then short-circuits on the already-existing prefix.
    for dir in &dirs {
        fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create directory '{}'", dir.display()))?;
    }

    // Largest first. The bundler appends the ~105 MB Node binary as the *last* entry, so any
    // static split of the work list hands it to one worker that is still writing long after
    // the rest have finished. Longest-job-first plus a shared cursor keeps every core busy to
    // the end.
    files.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(files.len().max(1));
    let next = AtomicUsize::new(0);

    let result: Result<()> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            let mut archive = archive.clone();
            let files = &files;
            let next = &next;
            handles.push(scope.spawn(move || -> Result<()> {
                // Reused across entries so the common small-file case is one allocation per
                // worker rather than one per file.
                let mut buf: Vec<u8> = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(planned) = files.get(i) else { break };
                    let path = &planned.path;

                    let mut entry = archive
                        .by_index(planned.index)
                        .context("Failed to read zip entry")?;
                    let mut out = fs::File::create(path)
                        .with_context(|| format!("Failed to create file {}", path.display()))?;

                    if planned.size <= SMALL_ENTRY_BYTES {
                        buf.clear();
                        buf.reserve(planned.size as usize);
                        entry
                            .read_to_end(&mut buf)
                            .with_context(|| format!("Failed to read {}", path.display()))?;
                        out.write_all(&buf).with_context(|| {
                            format!("Failed to write file to {}", path.display())
                        })?;
                    } else {
                        // A big entry through io::copy's 8 KiB loop is tens of thousands of
                        // write syscalls; buffer it instead.
                        let mut writer = std::io::BufWriter::with_capacity(1 << 20, &mut out);
                        std::io::copy(&mut entry, &mut writer).with_context(|| {
                            format!("Failed to write file to {}", path.display())
                        })?;
                        writer.flush().with_context(|| {
                            format!("Failed to flush file {}", path.display())
                        })?;
                    }

                    #[cfg(unix)]
                    {
                        if let Some(mode) = planned.mode {
                            use std::os::unix::fs::PermissionsExt;
                            // fchmod on the open descriptor: no second path resolution, and
                            // identical semantics to the previous path-based set_permissions.
                            out.set_permissions(std::fs::Permissions::from_mode(mode))
                                .with_context(|| {
                                    format!("Failed to set permissions on {}", path.display())
                                })?;
                        }
                    }
                }
                Ok(())
            }));
        }
        for handle in handles {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("extraction worker panicked"))??;
        }
        Ok(())
    });
    result?;

    Ok(())
}

/// Join a zip entry name onto `root`, rejecting absolute paths and traversal escapes.
///
/// Both `/` and `\` are treated as separators regardless of host platform. Zip names are
/// specified to use `/`, but `PathBuf::push` also splits on `\` under Windows — so accepting
/// a backslash as an ordinary character here would let an entry named `a\..\..\evil` escape
/// `root` on Windows while passing a component-wise `starts_with` check.
fn safe_join(root: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('\0') {
        return None;
    }

    let mut out = root.to_path_buf();
    let mut pushed = false;
    for component in name
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
    {
        if component == ".." {
            return None;
        }
        // Reject anything Windows would reinterpret: drive-relative prefixes, and trailing
        // dots or spaces which the Win32 layer silently strips.
        if component.contains(':') || component.ends_with('.') || component.ends_with(' ') {
            return None;
        }
        out.push(component);
        pushed = true;
    }

    if !pushed || !out.starts_with(root) {
        return None;
    }
    Some(out)
}

/// Start the bundled Node.js app. On Unix this *replaces* the current process, so it only
/// returns if Node could not be started at all.
fn run_app(app_dir: &Path, main_script: &str, args: &[std::ffi::OsString]) -> Result<()> {
    let app_path = app_dir.join("app");
    let node_executable = get_node_executable_path(app_dir);

    // Returning from here leaves the caller free to wipe and re-extract `app_dir`, so the
    // process must not be left with its cwd inside that directory.
    let previous_cwd = env::current_dir().ok();
    let restore_cwd = || {
        if let Some(cwd) = previous_cwd.as_ref() {
            let _ = env::set_current_dir(cwd);
        }
    };

    if env::set_current_dir(&app_path).is_err() {
        return Ok(());
    }

    let mut command = Command::new(&node_executable);
    command.arg(main_script).args(args);

    // Persist V8's compiled bytecode next to the extracted app so it survives between runs.
    // This is what closes the gap with bundlers that ship precompiled bytecode: without it
    // every launch recompiles the app's JavaScript, and the cost grows with dependency
    // count. Node < 22.1 simply ignores the variable, and a caller that sets it explicitly
    // keeps their own choice.
    if env::var_os("NODE_COMPILE_CACHE").is_none() {
        command.env("NODE_COMPILE_CACHE", app_dir.join(".v8cache"));
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Replaces this process image with Node: no fork, no resident parent holding the
        // launcher's address space for the lifetime of the app, and signals/exit status are
        // delivered by the kernel directly to Node.
        let err = command.exec();
        // exec() only returns on failure.
        restore_cwd();
        if node_executable.exists() {
            return Err(anyhow::anyhow!(err).context(format!(
                "Failed to execute Node.js at {}",
                node_executable.display()
            )));
        }
        return Ok(());
    }

    #[cfg(windows)]
    {
        use std::process::Stdio;
        let mut last_err: Option<std::io::Error> = None;
        for attempt in 1..=8u32 {
            match command
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
            {
                Ok(status) => std::process::exit(status.code().unwrap_or(1)),
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(50 * attempt as u64));
                }
            }
        }
        restore_cwd();
        if node_executable.exists() {
            if let Some(e) = last_err {
                return Err(anyhow::anyhow!(e).context(format!(
                    "Failed to execute Node.js at {}",
                    node_executable.display()
                )));
            }
        }
        Ok(())
    }
}

fn find_main_script(app_path: &Path) -> Result<String> {
    let package_json_path = app_path.join("package.json");

    if let Ok(content) = fs::read_to_string(&package_json_path) {
        if let Ok(package_json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(main) = package_json["main"].as_str() {
                if !main.trim().is_empty() {
                    return Ok(main.to_string());
                }
            }
        }
    }

    Ok("index.js".to_string())
}
