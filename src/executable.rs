use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use log::{error, info};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use uuid::Uuid;

use crate::embedded_template::EmbeddedTemplate;
use crate::platform::Platform;
use crate::rust_toolchain::RustToolchain;

/// Create a cross-platform Rust executable with embedded data while reporting progress to the provided ProgressBar if any0
pub fn create_self_extracting_executable_with_progress(
    output_path: &Path,
    zip_data: Vec<u8>,
    app_name: &str,
    progress: Option<&ProgressBar>,
) -> Result<()> {
    if let Err(e) = RustToolchain::check_availability() {
        error!("\nError: {e}");
        error!("{}", RustToolchain::get_installation_instructions());
        return Err(e);
    }

    let build_id = Uuid::new_v4().to_string();

    let temp_dir = TempDir::new().context("Failed to create temporary directory")?;
    let build_dir = temp_dir.path();

    copy_template_to_build_dir(build_dir)?;

    let zip_path = build_dir.join("embedded_data.zip");
    fs::write(&zip_path, &zip_data).context("Failed to write embedded zip data")?;

    let build_id_path = build_dir.join("build_id.txt");
    fs::write(&build_id_path, &build_id).context("Failed to write build ID")?;

    update_cargo_toml(build_dir, app_name)?;

    info!("Building native binary...");
    build_executable_with_progress(build_dir, output_path, app_name, progress)?;
    info!("Native binary built");

    Ok(())
}

fn copy_template_to_build_dir(build_dir: &Path) -> Result<()> {
    // Use embedded template files instead of filesystem copy
    let template = EmbeddedTemplate::new();
    template
        .write_to_dir(build_dir)
        .context("Failed to write embedded template files to build directory")?;

    Ok(())
}

fn update_cargo_toml(build_dir: &Path, app_name: &str) -> Result<()> {
    let cargo_toml_path = build_dir.join("Cargo.toml");
    let cargo_content =
        fs::read_to_string(&cargo_toml_path).context("Failed to read Cargo.toml")?;

    // Replace the package name
    let updated_content = cargo_content.replace(
        r#"name = "banderole-app""#,
        &format!(r#"name = "{}""#, sanitize_package_name(app_name)),
    );

    fs::write(&cargo_toml_path, updated_content).context("Failed to write updated Cargo.toml")?;

    Ok(())
}

fn sanitize_package_name(name: &str) -> String {
    // Rust package names must be valid identifiers
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_start_matches(|c: char| c.is_numeric() || c == '-')
        .to_string()
}

fn build_executable_with_progress(
    build_dir: &Path,
    output_path: &Path,
    app_name: &str,
    progress: Option<&ProgressBar>,
) -> Result<()> {
    let current_platform = Platform::current();
    let target_triple = get_target_triple(&current_platform);

    // Ensure we have the target installed
    install_rust_target(&target_triple)?;

    // Do not show a determinate bar until we know the total

    // Actual build; consume Cargo JSON messages to compute progress without a dry-run
    let mut cmd = Command::new("cargo");
    cmd.current_dir(build_dir)
        .args([
            "build",
            "--release",
            "--target",
            &target_triple,
            "--message-format",
            "json",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().context("Failed to execute cargo build")?;

    // Capture stdout/stderr for diagnostics; parse JSON on stdout for compiled count
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    let stdout_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let stderr_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

    // Spawn stdout reader + JSON progress parser
    let stdout_arc = Arc::clone(&stdout_buf);
    let pb_for_stdout = progress.cloned();
    let compiled_count = Arc::new(AtomicU64::new(0));
    let compiled_for_stdout = Arc::clone(&compiled_count);
    // Determine total crates using cargo metadata (no dry run, no stderr parsing)
    // Determine total first, before spawning cargo; don't show bar until known
    let known_total: u64 = compute_total_via_cargo_metadata(build_dir, &target_triple).unwrap_or(0);
    // Determine total compile units using cargo metadata; only then show a determinate bar
    if let Some(pb) = progress {
        if known_total > 0 {
            pb.set_style(
                ProgressStyle::with_template(
                    "[ {wide_bar} ] {pos}/{len}",
                )
                .unwrap()
                .progress_chars("#>-"),
            );
            pb.set_length(known_total);
            pb.set_position(0);
        }
    }

    let stdout_handle = child.stdout.take().map(|stdout| {
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let reader = BufReader::new(stdout);
            let mut total_artifacts: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let mut compiled_artifacts: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for line in reader.lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => break,
                };
                if let Ok(mut buf) = stdout_arc.lock() {
                    buf.push_str(&line);
                    buf.push('\n');
                }
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                    if let Some(reason) = value.get("reason").and_then(|r| r.as_str()) {
                        if reason == "compiler-artifact" {
                            let pkg = value
                                .get("package_id")
                                .and_then(|p| p.as_str())
                                .unwrap_or("");
                            let target_name = value
                                .get("target")
                                .and_then(|t| t.get("name"))
                                .and_then(|n| n.as_str())
                                .unwrap_or("");
                            let key = format!("{pkg}:{target_name}");
                            total_artifacts.insert(key.clone());
                            let is_fresh = value
                                .get("fresh")
                                .and_then(|f| f.as_bool())
                                .unwrap_or(false);
                            if !is_fresh {
                                compiled_artifacts.insert(key);
                            }

                            // Update compiled counter and progress position
                            let compiled_now = compiled_artifacts.len() as u64;
                            compiled_for_stdout.store(compiled_now, Ordering::SeqCst);
                            if let Some(pb) = &pb_for_stdout {
                                let total_len = known_total;
                                if total_len > 0 && pb.length().unwrap_or(0) != total_len {
                                    pb.set_length(total_len);
                                }
                                let pos = if total_len > 0 {
                                    compiled_now.min(total_len)
                                } else {
                                    compiled_now
                                };
                                pb.set_position(pos);
                            }
                        }
                    }
                }
            }
        })
    });

    // Spawn stderr reader (diagnostics only)
    let stderr_arc = Arc::clone(&stderr_buf);
    let stderr_handle = child.stderr.take().map(|stderr| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut reader = std::io::BufReader::new(stderr);
            let mut capture_bytes: Vec<u8> = Vec::new();
            let _ = reader.read_to_end(&mut capture_bytes);
            if let Ok(mut buf) = stderr_arc.lock() {
                match String::from_utf8(capture_bytes) {
                    Ok(s) => buf.push_str(&s),
                    Err(_) => buf.push_str("<non-utf8 stderr>"),
                }
            }
        })
    });

    // Consume JSON messages from stdout; estimate total as number of artifacts and compiled as non-fresh artifacts
    if let Some(stdout) = child.stdout.take() {
        use std::io::{BufRead, BufReader};
        let reader = BufReader::new(stdout);
        let mut total_artifacts: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut compiled_artifacts: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let Some(reason) = value.get("reason").and_then(|r| r.as_str()) else {
                continue;
            };
            match reason {
                "compiler-artifact" => {
                    let pkg = value
                        .get("package_id")
                        .and_then(|p| p.as_str())
                        .unwrap_or("");
                    let tname = value
                        .get("target")
                        .and_then(|t| t.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    let key = format!("{pkg}:{tname}");
                    total_artifacts.insert(key.clone());
                    let is_fresh = value
                        .get("fresh")
                        .and_then(|f| f.as_bool())
                        .unwrap_or(false);
                    if !is_fresh {
                        compiled_artifacts.insert(key);
                    }

                    if let Some(pb) = progress {
                        let total_now = total_artifacts.len() as u64;
                        if total_now > 0 && pb.length().unwrap_or(0) != total_now {
                            pb.set_length(total_now);
                        }
                        let compiled_now = compiled_artifacts.len() as u64;
                        if compiled_now <= pb.length().unwrap_or(0) {
                            pb.set_position(compiled_now);
                        }

                        if let Some(name) = value
                            .get("target")
                            .and_then(|t| t.get("name"))
                            .and_then(|n| n.as_str())
                        {
                            let len = pb.length().unwrap_or(0);
                            let pos = pb.position();
                            if len > 1 {
                                pb.set_message(format!("Compiling binary: {name} ({pos}/{len})"));
                            } else {
                                pb.set_message(format!("Compiling binary: {name}"));
                            }
                        }
                    }
                }
                "build-finished" => {
                    if let Some(pb) = progress {
                        if let Some(len) = pb.length() {
                            if len > 0 {
                                pb.set_position(len);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let status = child.wait().context("Failed to wait for cargo build")?;
    if let Some(h) = stdout_handle {
        let _ = h.join();
    }
    if let Some(h) = stderr_handle {
        let _ = h.join();
    }
    if !status.success() {
        let out = stdout_buf
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_default();
        let err = stderr_buf
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_default();
        let trim_tail = |mut s: String| {
            const MAX: usize = 4000;
            if s.len() > MAX {
                s.split_off(s.len() - MAX)
            } else {
                String::new()
            };
            if s.len() > MAX {
                s[s.len() - MAX..].to_string()
            } else {
                s
            }
        };
        let out_tail = trim_tail(out);
        let err_tail = trim_tail(err);
        anyhow::bail!(
            "Cargo build failed.\nLast stdout:\n{}\nLast stderr:\n{}",
            out_tail,
            err_tail
        );
    }

    // Get the sanitized package name to find the correct executable
    let package_name = sanitize_package_name(app_name);
    let executable_name = if current_platform.is_windows() {
        format!("{package_name}.exe")
    } else {
        package_name
    };

    let built_executable = build_dir
        .join("target")
        .join(&target_triple)
        .join("release")
        .join(executable_name);

    if !built_executable.exists() {
        anyhow::bail!(
            "Built executable not found at {}",
            built_executable.display()
        );
    }

    // Ensure output directory exists
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).context("Failed to create output directory")?;
    }

    fs::copy(&built_executable, output_path)
        .context("Failed to copy built executable to output path")?;

    // Set executable permissions on Unix systems
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(output_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(output_path, perms)?;
    }

    Ok(())
}

fn compute_total_via_cargo_metadata(build_dir: &Path, target_triple: &str) -> Result<u64> {
    // Strategy: union of host + target resolve nodes, then count compile-relevant targets per package
    // Relevant targets: lib, proc-macro, custom-build for all packages; bin only for the root package

    fn run_metadata(build_dir: &Path, args: &[&str]) -> Result<serde_json::Value> {
        let output = Command::new("cargo")
            .current_dir(build_dir)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run cargo {}", args.join(" ")))?;
        if !output.status.success() {
            anyhow::bail!(
                "cargo {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let v: serde_json::Value = serde_json::from_slice(&output.stdout)
            .context("Failed to parse cargo metadata JSON")?;
        Ok(v)
    }

    fn get_host_triple() -> Result<String> {
        let output = Command::new("rustc")
            .arg("-vV")
            .output()
            .context("Failed to run rustc -vV")?;
        if !output.status.success() {
            anyhow::bail!(
                "rustc -vV failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Some(rest) = line.strip_prefix("host: ") {
                return Ok(rest.trim().to_string());
            }
        }
        anyhow::bail!("Failed to parse host triple from rustc -vV")
    }

    // Run three metadata queries: target-filtered, host-filtered, and unfiltered for packages map
    let meta_target = run_metadata(
        build_dir,
        &[
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            target_triple,
        ],
    )?;
    let host_triple = get_host_triple().unwrap_or_else(|_| target_triple.to_string());
    let meta_host = run_metadata(
        build_dir,
        &[
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            &host_triple,
        ],
    )?;
    let meta_all = run_metadata(build_dir, &["metadata", "--format-version", "1"])?;

    // Collect union of package ids to be considered
    let mut pkg_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let push_ids = |val: &serde_json::Value, set: &mut std::collections::HashSet<String>| {
        if let Some(nodes) = val
            .get("resolve")
            .and_then(|r| r.get("nodes"))
            .and_then(|n| n.as_array())
        {
            for node in nodes {
                if let Some(id) = node.get("id").and_then(|i| i.as_str()) {
                    set.insert(id.to_string());
                }
            }
        }
    };
    push_ids(&meta_target, &mut pkg_ids);
    push_ids(&meta_host, &mut pkg_ids);

    // Build package map from unfiltered metadata
    let mut packages_by_id: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();
    if let Some(packages) = meta_all.get("packages").and_then(|p| p.as_array()) {
        for p in packages {
            if let Some(id) = p.get("id").and_then(|i| i.as_str()) {
                packages_by_id.insert(id.to_string(), p.clone());
            }
        }
    }

    // Root package id
    let root_id = meta_target
        .get("resolve")
        .and_then(|r| r.get("root"))
        .and_then(|r| r.as_str())
        .or_else(|| {
            meta_all
                .get("resolve")
                .and_then(|r| r.get("root"))
                .and_then(|r| r.as_str())
        })
        .map(|s| s.to_string());

    let mut total_units: u64 = 0;
    for pid in pkg_ids {
        let Some(pkg) = packages_by_id.get(&pid) else {
            continue;
        };
        let is_root = root_id.as_ref().is_some_and(|r| r == &pid);
        if let Some(targets) = pkg.get("targets").and_then(|t| t.as_array()) {
            for t in targets {
                let has_kind = |name: &str| -> bool {
                    t.get("kind")
                        .and_then(|k| k.as_array())
                        .is_some_and(|kinds| kinds.iter().any(|v| v.as_str() == Some(name)))
                };
                if has_kind("custom-build") {
                    total_units += 1;
                    continue;
                }
                if has_kind("proc-macro") {
                    total_units += 1;
                    continue;
                }
                if has_kind("lib") {
                    total_units += 1;
                    continue;
                }
                if is_root && has_kind("bin") {
                    total_units += 1;
                    continue;
                }
            }
        }
    }

    if total_units == 0 {
        // Fallback to node counts if our logic fails
        let nodes_len = meta_target
            .get("resolve")
            .and_then(|r| r.get("nodes"))
            .and_then(|n| n.as_array())
            .map(|a| a.len() as u64)
            .unwrap_or(1);
        return Ok(nodes_len.max(1));
    }

    Ok(total_units)
}

fn get_target_triple(platform: &Platform) -> String {
    match platform {
        Platform::MacosX64 => "x86_64-apple-darwin".to_string(),
        Platform::MacosArm64 => "aarch64-apple-darwin".to_string(),
        Platform::LinuxX64 => "x86_64-unknown-linux-gnu".to_string(),
        Platform::LinuxArm64 => "aarch64-unknown-linux-gnu".to_string(),
        Platform::WindowsX64 => "x86_64-pc-windows-msvc".to_string(),
        Platform::WindowsArm64 => "aarch64-pc-windows-msvc".to_string(),
    }
}

fn install_rust_target(target: &str) -> Result<()> {
    RustToolchain::ensure_target_installed(target)
}
