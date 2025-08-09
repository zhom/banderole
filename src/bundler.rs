use crate::executable;
use crate::node_downloader::NodeDownloader;
use crate::node_version_manager::NodeVersionManager;
use crate::platform::Platform;
use anyhow::{Context, Result};
use console::{style, Emoji};
use indicatif::{HumanDuration, MultiProgress, ProgressBar, ProgressStyle};
use log::{debug, info, warn};
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use zip::ZipWriter;

/// Public entry-point used by `main.rs`.
///
/// * `project_path` – path that contains a `package.json`.
/// * `output_path`  – optional path to the produced bundle file. If omitted, an
///   automatically-generated name is used.
/// * `custom_name` – optional custom name for the executable.
/// * `no_compression` – disable compression for faster bundling (useful for testing).
///
/// The implementation uses a simpler, more reliable approach based on Playwright's bundling strategy.
pub async fn bundle_project(
    project_path: PathBuf,
    output_path: Option<PathBuf>,
    custom_name: Option<String>,
    no_compression: bool,
    ignore_cached_versions: bool,
    multi: &MultiProgress,
) -> Result<()> {
    let project_path = project_path
        .canonicalize()
        .context("Failed to resolve project path")?;
    let pkg_json = project_path.join("package.json");
    anyhow::ensure!(
        pkg_json.exists(),
        "package.json not found in {}",
        project_path.display()
    );

    let package_content = fs::read_to_string(&pkg_json).context("Failed to read package.json")?;
    let package_value: Value =
        serde_json::from_str(&package_content).context("Failed to parse package.json")?;

    let (app_name, app_version) = (
        package_value["name"].as_str().unwrap_or("app").to_string(),
        package_value["version"]
            .as_str()
            .unwrap_or("0.0.0")
            .to_string(),
    );

    let source_dir = determine_source_directory(&project_path, &package_value)?;

    let node_version =
        detect_node_version_with_workspace_support(&project_path, ignore_cached_versions)
            .await
            .unwrap_or_else(|_| "22.17.1".into());

    info!(
        "Preparing build for {app_name} v{app_version} (Node {node_version}, {plat})",
        plat = Platform::current()
    );

    if source_dir != project_path {
        debug!("Using source directory: {}", source_dir.display());
    }

    let output_path = resolve_output_path(output_path, &app_name, custom_name.as_deref())?;

    // Styles
    let spinner_style =
        ProgressStyle::with_template("{prefix:.bold.dim} {spinner:.green} {wide_msg}")
            .unwrap()
            .tick_chars("/|\\- ");
    let bar_style = ProgressStyle::with_template(
        "{prefix:.bold.dim} {msg}[ {wide_bar} ] {pos}/{len}",
    )
    .unwrap()
    .progress_chars("#>-");

    let emoji_prepare = Emoji("🔧", "");
    let emoji_bundle = Emoji("📦", "");
    let emoji_build = Emoji("⚙️ ", "");
    let emoji_done = Emoji("✨ ", "");
    let started = Instant::now();

    // Stage 1: Prepare environment (resolve version + Node ready)
    println!(
        "{} {} Preparing environment...",
        style("[1/3]").bold().dim(),
        emoji_prepare
    );
    let pb_prepare = multi.add(ProgressBar::new_spinner());
    pb_prepare.set_style(spinner_style.clone());

    let node_downloader = NodeDownloader::new_with_persistent_cache(&node_version).await?;
    let node_executable = node_downloader
        .ensure_node_binary_with_progress(Some(&pb_prepare))
        .await?;
    let node_root = node_executable
        .parent()
        .expect("node executable must have a parent")
        .parent()
        .unwrap_or_else(|| panic!("Unexpected node layout for {}", node_executable.display()));
    pb_prepare.finish_and_clear();

    // Stage 2: Bundle application into archive
    println!(
        "{} {} Bundling application...",
        style("[2/3]").bold().dim(),
        emoji_bundle
    );
    let pb_bundle = multi.add(ProgressBar::new(0));
    pb_bundle.set_style(bar_style.clone());

    let mut zip_data: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let opts: zip::write::FileOptions<'static, ()> = if no_compression {
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored)
        } else {
            zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(8))
        };

        // Pre-count app files
        let app_files = count_files_in_dir(&source_dir, true, true);
        pb_bundle.set_length(app_files);
        add_dir_to_zip_excluding_node_modules(
            &mut zip,
            &source_dir,
            Path::new("app"),
            opts,
            Some(&pb_bundle),
        )?;

        // Dependencies will extend the total as we discover them
        bundle_dependencies(
            &mut zip,
            &project_path,
            &source_dir,
            &package_value,
            opts,
            Some(&pb_bundle),
        )?;

        // Count node runtime files and extend length
        let node_files = count_files_in_dir(node_root, false, true);
        let new_len = pb_bundle.length().unwrap_or(0) + node_files;
        pb_bundle.set_length(new_len);
        add_dir_to_zip(
            &mut zip,
            node_root,
            Path::new("node"),
            opts,
            Some(&pb_bundle),
        )?;
        zip.finish()?;
    }
    pb_bundle.finish_and_clear();

    // Stage 3: Create executable
    println!(
        "{} {} Building native binary...",
        style("[3/3]").bold().dim(),
        emoji_build
    );
    let pb_build = multi.add(ProgressBar::new(0));
    // Do not show a determinate bar yet; use a spinner until total is known
    pb_build.set_style(spinner_style.clone());

    executable::create_self_extracting_executable_with_progress(
        &output_path,
        zip_data,
        &app_name,
        Some(&pb_build),
    )?;
    pb_build.finish_and_clear();

    println!(
        "{} Done in {}",
        emoji_done,
        HumanDuration(started.elapsed())
    );

    info!("Bundle created at {}", output_path.display());
    Ok(())
}

// Count files (and symlinks) in a directory. Optionally exclude top-level node_modules.
fn count_files_in_dir(dir: &Path, exclude_node_modules: bool, follow_links: bool) -> u64 {
    let mut count = 0u64;
    let walker = if follow_links {
        walkdir::WalkDir::new(dir).follow_links(true)
    } else {
        walkdir::WalkDir::new(dir).follow_links(false)
    };
    for entry in walker.into_iter().flatten() {
        let path = entry.path();
        if exclude_node_modules {
            if let Ok(rel) = path.strip_prefix(dir) {
                if rel
                    .components()
                    .next()
                    .is_some_and(|c| c.as_os_str() == "node_modules")
                {
                    continue;
                }
            }
        }
        if entry.file_type().is_file() || entry.file_type().is_symlink() {
            count += 1;
        }
    }
    count
}

/// Bundle dependencies with improved package manager support
fn bundle_dependencies<W>(
    zip: &mut ZipWriter<W>,
    project_path: &Path,
    source_dir: &Path,
    _package_value: &Value,
    opts: zip::write::FileOptions<'static, ()>,
    progress: Option<&ProgressBar>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    if source_dir != project_path {
        let root_package_json = project_path.join("package.json");
        if root_package_json.exists() {
            zip.start_file("app/package.json", opts)?;

            let content = fs::read_to_string(&root_package_json)
                .context("Failed to read root package.json")?;
            let mut package_value: Value =
                serde_json::from_str(&content).context("Failed to parse root package.json")?;

            if let Some(main) = package_value["main"].as_str() {
                let main_path = project_path.join(main);
                if let Ok(relative_to_source) = main_path.strip_prefix(source_dir) {
                    package_value["main"] =
                        Value::String(relative_to_source.to_string_lossy().to_string());
                }
            }

            let modified_content = serde_json::to_string_pretty(&package_value)
                .context("Failed to serialize modified package.json")?;
            zip.write_all(modified_content.as_bytes())?;
        }
    }

    let deps_result = find_and_bundle_dependencies(zip, project_path, opts, progress)?;

    if deps_result.dependencies_found {
        debug!("Bundled dependencies: {}", deps_result.source_description);
    } else {
        debug!("No dependencies found to bundle");
    }

    for warning in &deps_result.warnings {
        debug!("{warning}");
    }

    Ok(())
}

struct DependenciesResult {
    dependencies_found: bool,
    source_description: String,
    warnings: Vec<String>,
}

/// Find and bundle dependencies with support for different package managers and workspace configurations
fn find_and_bundle_dependencies<W>(
    zip: &mut ZipWriter<W>,
    project_path: &Path,
    opts: zip::write::FileOptions<'static, ()>,
    progress: Option<&ProgressBar>,
) -> Result<DependenciesResult>
where
    W: Write + Read + std::io::Seek,
{
    let mut warnings = Vec::new();

    // Strategy 1: Check for node_modules in the project directory
    let project_node_modules = project_path.join("node_modules");
    if project_node_modules.exists() {
        let package_manager = detect_package_manager(&project_node_modules, project_path);

        let is_pnpm_workspace = if package_manager == PackageManager::Pnpm {
            if let Ok(entries) = fs::read_dir(&project_node_modules) {
                entries.flatten().any(|entry| {
                    if entry.file_type().ok().is_some_and(|ft| ft.is_symlink()) {
                        if let Ok(target) = fs::read_link(entry.path()) {
                            let target_str = target.to_string_lossy();
                            target_str.contains("/.pnpm/") && target_str.starts_with("../")
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                })
            } else {
                false
            }
        } else {
            false
        };

        if !is_pnpm_workspace {
            match package_manager {
                PackageManager::Pnpm => {
                    bundle_pnpm_dependencies(zip, project_path, opts, progress)?;
                    return Ok(DependenciesResult {
                        dependencies_found: true,
                        source_description: "pnpm dependencies (node_modules + .pnpm)".to_string(),
                        warnings,
                    });
                }
                PackageManager::Yarn => {
                    bundle_node_modules_comprehensive(
                        zip,
                        &project_node_modules,
                        project_path,
                        opts,
                        progress,
                    )?;
                    return Ok(DependenciesResult {
                        dependencies_found: true,
                        source_description: "yarn dependencies (node_modules)".to_string(),
                        warnings,
                    });
                }
                PackageManager::Npm | PackageManager::Unknown => {
                    bundle_node_modules_comprehensive(
                        zip,
                        &project_node_modules,
                        project_path,
                        opts,
                        progress,
                    )?;
                    return Ok(DependenciesResult {
                        dependencies_found: true,
                        source_description: "npm dependencies (node_modules)".to_string(),
                        warnings,
                    });
                }
            }
        }
    }

    // Strategy 2: Check for workspace scenario - look in parent directories
    let mut current_path = project_path.parent();
    while let Some(parent_path) = current_path {
        let parent_node_modules = parent_path.join("node_modules");
        let parent_package_json = parent_path.join("package.json");

        if parent_node_modules.exists() && parent_package_json.exists() {
            let mut is_workspace = false;

            if let Ok(content) = fs::read_to_string(&parent_package_json) {
                if let Ok(pkg_value) = serde_json::from_str::<Value>(&content) {
                    is_workspace = pkg_value["workspaces"].is_array()
                        || pkg_value["workspaces"]["packages"].is_array()
                        || pkg_value["workspaces"].is_object();
                }
            }

            let pnpm_workspace_yaml = parent_path.join("pnpm-workspace.yaml");
            if !is_workspace && pnpm_workspace_yaml.exists() {
                is_workspace = true;
            }

            if is_workspace {
                warnings.push(format!(
                    "Found workspace dependencies in parent directory: {}",
                    parent_path.display()
                ));

                let package_manager = detect_package_manager(&parent_node_modules, parent_path);

                match package_manager {
                    PackageManager::Pnpm => {
                        bundle_pnpm_workspace_dependencies(
                            zip,
                            parent_path,
                            project_path,
                            opts,
                            progress,
                        )?;
                        return Ok(DependenciesResult {
                            dependencies_found: true,
                            source_description: format!(
                                "workspace pnpm dependencies from {}",
                                parent_path.display()
                            ),
                            warnings,
                        });
                    }
                    _ => {
                        bundle_workspace_dependencies(
                            zip,
                            &parent_node_modules,
                            parent_path,
                            project_path,
                            opts,
                            progress,
                        )?;
                        return Ok(DependenciesResult {
                            dependencies_found: true,
                            source_description: format!(
                                "workspace dependencies from {}",
                                parent_path.display()
                            ),
                            warnings,
                        });
                    }
                }
            }
        }

        current_path = parent_path.parent();

        if parent_path.components().count() < 2 {
            break;
        }
    }

    Ok(DependenciesResult {
        dependencies_found: false,
        source_description: "no dependencies found".to_string(),
        warnings,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PackageManager {
    Npm,
    Yarn,
    Pnpm,
    Unknown,
}

/// Detect the package manager based on the node_modules structure and lockfiles
fn detect_package_manager(node_modules_path: &Path, project_path: &Path) -> PackageManager {
    if node_modules_path.join(".pnpm").exists() {
        return PackageManager::Pnpm;
    }

    if node_modules_path.exists() {
        if let Ok(entries) = fs::read_dir(node_modules_path) {
            for entry in entries.flatten() {
                if entry.file_type().ok().is_some_and(|ft| ft.is_symlink()) {
                    if let Ok(target) = fs::read_link(entry.path()) {
                        let target_str = target.to_string_lossy();
                        if target_str.contains("/.pnpm/") {
                            return PackageManager::Pnpm;
                        }
                    }
                }
            }
        }
    }

    if project_path.join("pnpm-lock.yaml").exists() {
        return PackageManager::Pnpm;
    }

    if project_path.join("yarn.lock").exists() {
        return PackageManager::Yarn;
    }

    if project_path.join("package-lock.json").exists() {
        return PackageManager::Npm;
    }

    PackageManager::Unknown
}

/// Bundle pnpm dependencies by creating a flattened node_modules structure
fn bundle_pnpm_dependencies<W>(
    zip: &mut ZipWriter<W>,
    project_path: &Path,
    opts: zip::write::FileOptions<'static, ()>,
    progress: Option<&ProgressBar>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    let node_modules_path = project_path.join("node_modules");
    let pnpm_dir = node_modules_path.join(".pnpm");

    if !pnpm_dir.exists() {
        if node_modules_path.exists() {
            if let Some(pb) = progress {
                pb.set_length(
                    pb.length().unwrap_or(0) + count_files_in_dir(&node_modules_path, false, false),
                );
            }
            add_dir_to_zip_no_follow(
                zip,
                &node_modules_path,
                Path::new("app/node_modules"),
                opts,
                progress,
            )?;
        }
        return Ok(());
    }

    let mut packages_to_bundle = std::collections::HashSet::new();

    let package_json_path = project_path.join("package.json");
    if let Ok(package_json_content) = fs::read_to_string(&package_json_path) {
        if let Ok(package_json) = serde_json::from_str::<Value>(&package_json_content) {
            if let Some(deps) = package_json["dependencies"].as_object() {
                for dep_name in deps.keys() {
                    packages_to_bundle.insert(dep_name.clone());
                }
            }
            // Only include devDependencies if they're actually used in production
            // For now, skip them to keep the bundle smaller
        }
    }

    let mut resolved_packages = std::collections::HashSet::new();
    for package_name in &packages_to_bundle {
        resolve_package_dependencies(
            &node_modules_path,
            &pnpm_dir,
            package_name,
            &mut resolved_packages,
            0, // depth
        )?;
    }

    debug!(
        "Bundling {} packages (resolved dependencies) for pnpm project",
        resolved_packages.len()
    );

    zip.add_directory("app/node_modules/", opts)?;

    for package_name in &resolved_packages {
        if let Err(e) = copy_pnpm_package_comprehensive(
            zip,
            &node_modules_path,
            &pnpm_dir,
            package_name,
            opts,
            progress,
        ) {
            warn!("Failed to copy package {package_name}: {e}");
        }
    }

    let bin_dir = node_modules_path.join(".bin");
    if bin_dir.exists() {
        if let Some(pb) = progress {
            pb.set_length(pb.length().unwrap_or(0) + count_files_in_dir(&bin_dir, false, false));
        }
        add_dir_to_zip_no_follow(
            zip,
            &bin_dir,
            Path::new("app/node_modules/.bin"),
            opts,
            progress,
        )?;
    }

    let important_files = [".modules.yaml", ".pnpm-workspace-state-v1.json"];
    for file_name in important_files {
        let file_path = node_modules_path.join(file_name);
        if file_path.exists() {
            let dest_path = Path::new("app/node_modules").join(file_name);
            zip.start_file(dest_path.to_string_lossy().as_ref(), opts)?;
            let data = fs::read(&file_path)?;
            zip.write_all(&data)?;
        }
    }

    Ok(())
}

/// Recursively resolve dependencies for a package
fn resolve_package_dependencies(
    node_modules_path: &Path,
    pnpm_dir: &Path,
    package_name: &str,
    resolved: &mut std::collections::HashSet<String>,
    depth: usize,
) -> Result<()> {
    // Avoid infinite recursion
    if depth > 20 {
        return Ok(());
    }

    if resolved.contains(package_name) {
        return Ok(());
    }

    resolved.insert(package_name.to_string());

    let package_json_content =
        match find_package_json_content(node_modules_path, pnpm_dir, package_name) {
            Ok(content) => content,
            Err(_) => return Ok(()), // Skip packages we can't find
        };

    if let Ok(package_json) = serde_json::from_str::<Value>(&package_json_content) {
        if let Some(deps) = package_json["dependencies"].as_object() {
            for dep_name in deps.keys() {
                resolve_package_dependencies(
                    node_modules_path,
                    pnpm_dir,
                    dep_name,
                    resolved,
                    depth + 1,
                )?;
            }
        }

        if let Some(peer_deps) = package_json["peerDependencies"].as_object() {
            for dep_name in peer_deps.keys() {
                if package_exists_in_pnpm(node_modules_path, pnpm_dir, dep_name) {
                    resolve_package_dependencies(
                        node_modules_path,
                        pnpm_dir,
                        dep_name,
                        resolved,
                        depth + 1,
                    )?;
                }
            }
        }

        if let Some(optional_deps) = package_json["optionalDependencies"].as_object() {
            for dep_name in optional_deps.keys() {
                if package_exists_in_pnpm(node_modules_path, pnpm_dir, dep_name) {
                    resolve_package_dependencies(
                        node_modules_path,
                        pnpm_dir,
                        dep_name,
                        resolved,
                        depth + 1,
                    )?;
                }
            }
        }
    }

    Ok(())
}

/// Find package.json content for a package
fn find_package_json_content(
    node_modules_path: &Path,
    pnpm_dir: &Path,
    package_name: &str,
) -> Result<String> {
    let top_level_package = node_modules_path.join(package_name);
    if top_level_package.exists() {
        let target_path = if top_level_package.is_symlink() {
            let target = fs::read_link(&top_level_package)?;
            if target.is_absolute() {
                target
            } else {
                top_level_package
                    .parent()
                    .unwrap()
                    .join(target)
                    .canonicalize()?
            }
        } else {
            top_level_package
        };

        let package_json_path = target_path.join("package.json");
        if package_json_path.exists() {
            return fs::read_to_string(&package_json_path).context("Failed to read package.json");
        }
    }

    for entry in fs::read_dir(pnpm_dir)? {
        let entry = entry?;
        let pnpm_package_name = entry.file_name().to_string_lossy().to_string();

        if let Some(extracted_name) = extract_package_name_from_pnpm(&pnpm_package_name) {
            if extracted_name == package_name {
                let pnpm_package_path = entry.path().join("node_modules").join(package_name);
                let package_json_path = pnpm_package_path.join("package.json");
                if package_json_path.exists() {
                    return fs::read_to_string(&package_json_path)
                        .context("Failed to read package.json");
                }
            }
        }
    }

    anyhow::bail!("Could not find package.json for {}", package_name)
}

/// Check if a package exists in the pnpm structure
fn package_exists_in_pnpm(node_modules_path: &Path, pnpm_dir: &Path, package_name: &str) -> bool {
    if node_modules_path.join(package_name).exists() {
        return true;
    }

    if let Ok(entries) = fs::read_dir(pnpm_dir) {
        for entry in entries.flatten() {
            let pnpm_package_name = entry.file_name().to_string_lossy().to_string();
            if let Some(extracted_name) = extract_package_name_from_pnpm(&pnpm_package_name) {
                if extracted_name == package_name {
                    return true;
                }
            }
        }
    }

    false
}

/// Extract package name from pnpm directory name (e.g., "adm-zip@0.5.16" -> "adm-zip")
fn extract_package_name_from_pnpm(pnpm_name: &str) -> Option<String> {
    if pnpm_name.starts_with('@') {
        if let Some(at_pos) = pnpm_name.rfind('@') {
            if at_pos > 0 {
                let package_part = &pnpm_name[..at_pos];
                return Some(package_part.replace('+', "/"));
            }
        }
        return Some(pnpm_name.replace('+', "/"));
    }
    if let Some(at_pos) = pnpm_name.find('@') {
        Some(pnpm_name[..at_pos].to_string())
    } else {
        Some(pnpm_name.to_string())
    }
}

/// Copy a package, trying both top-level and .pnpm locations
fn copy_pnpm_package_comprehensive<W>(
    zip: &mut ZipWriter<W>,
    node_modules_path: &Path,
    pnpm_dir: &Path,
    package_name: &str,
    opts: zip::write::FileOptions<'static, ()>,
    progress: Option<&ProgressBar>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    let dest_path = Path::new("app/node_modules").join(package_name);

    let top_level_package = node_modules_path.join(package_name);
    if top_level_package.exists() {
        let target_path = if top_level_package.is_symlink() {
            let target = fs::read_link(&top_level_package)?;
            if target.is_absolute() {
                target
            } else {
                top_level_package
                    .parent()
                    .unwrap()
                    .join(target)
                    .canonicalize()?
            }
        } else {
            top_level_package
        };

        if target_path.exists() {
            if let Some(pb) = progress {
                pb.set_length(
                    pb.length().unwrap_or(0) + count_files_in_dir(&target_path, false, false),
                );
            }
            add_dir_to_zip_no_follow_skip_parents(zip, &target_path, &dest_path, opts, progress)?;
            return Ok(());
        }
    }
    for entry in fs::read_dir(pnpm_dir)? {
        let entry = entry?;
        let pnpm_package_name = entry.file_name().to_string_lossy().to_string();
        if let Some(extracted_name) = extract_package_name_from_pnpm(&pnpm_package_name) {
            if extracted_name == package_name {
                let pnpm_package_path = entry.path().join("node_modules").join(package_name);
                if pnpm_package_path.exists() {
                    if let Some(pb) = progress {
                        pb.set_length(
                            pb.length().unwrap_or(0)
                                + count_files_in_dir(&pnpm_package_path, false, false),
                        );
                    }
                    add_dir_to_zip_no_follow_skip_parents(
                        zip,
                        &pnpm_package_path,
                        &dest_path,
                        opts,
                        progress,
                    )?;
                    return Ok(());
                }
            }
        }
    }

    Ok(())
}

/// Bundle node_modules with comprehensive dependency resolution
fn bundle_node_modules_comprehensive<W>(
    zip: &mut ZipWriter<W>,
    node_modules_path: &Path,
    project_path: &Path,
    opts: zip::write::FileOptions<'static, ()>,
    progress: Option<&ProgressBar>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    let mut packages_to_bundle = std::collections::HashSet::new();

    let package_json_path = project_path.join("package.json");
    if let Ok(package_json_content) = fs::read_to_string(&package_json_path) {
        if let Ok(package_json) = serde_json::from_str::<Value>(&package_json_content) {
            if let Some(deps) = package_json["dependencies"].as_object() {
                for dep_name in deps.keys() {
                    packages_to_bundle.insert(dep_name.clone());
                }
            }
            if let Some(peer_deps) = package_json["peerDependencies"].as_object() {
                for dep_name in peer_deps.keys() {
                    packages_to_bundle.insert(dep_name.clone());
                }
            }
            if let Some(optional_deps) = package_json["optionalDependencies"].as_object() {
                for dep_name in optional_deps.keys() {
                    packages_to_bundle.insert(dep_name.clone());
                }
            }
        }
    }

    let pnpm_dir = node_modules_path.join(".pnpm");
    if pnpm_dir.exists() {
        let mut resolved_packages = std::collections::HashSet::new();
        for package_name in &packages_to_bundle {
            resolve_package_dependencies(
                node_modules_path,
                &pnpm_dir,
                package_name,
                &mut resolved_packages,
                0,
            )?;
        }

        debug!(
            "Bundling {} packages (resolved dependencies) for pnpm node_modules",
            resolved_packages.len()
        );

        zip.add_directory("app/node_modules/", opts)?;

        for package_name in &resolved_packages {
            if let Err(e) = copy_pnpm_package_comprehensive(
                zip,
                node_modules_path,
                &pnpm_dir,
                package_name,
                opts,
                progress,
            ) {
                warn!("Failed to copy package {package_name}: {e}");
            }
        }
    } else {
        let mut resolved_packages = std::collections::HashSet::new();
        for package_name in &packages_to_bundle {
            resolve_workspace_dependencies(
                node_modules_path,
                package_name,
                &mut resolved_packages,
                0,
            )?;
        }

        debug!(
            "Bundling {} packages (resolved dependencies) for regular node_modules",
            resolved_packages.len()
        );

        zip.add_directory("app/node_modules/", opts)?;

        for package_name in &resolved_packages {
            if let Err(e) =
                copy_workspace_package(zip, node_modules_path, package_name, opts, progress)
            {
                warn!("Failed to copy package {package_name}: {e}");
            }
        }
    }

    let bin_dir = node_modules_path.join(".bin");
    if bin_dir.exists() {
        if let Some(pb) = progress {
            pb.set_length(pb.length().unwrap_or(0) + count_files_in_dir(&bin_dir, false, false));
        }
        add_dir_to_zip_no_follow(
            zip,
            &bin_dir,
            Path::new("app/node_modules/.bin"),
            opts,
            progress,
        )?;
    }

    let important_files = [".modules.yaml", ".pnpm-workspace-state-v1.json"];
    for file_name in important_files {
        let file_path = node_modules_path.join(file_name);
        if file_path.exists() {
            let dest_path = Path::new("app/node_modules").join(file_name);
            zip.start_file(dest_path.to_string_lossy().as_ref(), opts)?;
            let data = fs::read(&file_path)?;
            zip.write_all(&data)?;
            if let Some(pb) = progress {
                pb.inc(1);
            }
        }
    }

    Ok(())
}

/// Bundle workspace dependencies (node_modules from parent)
fn bundle_workspace_dependencies<W>(
    zip: &mut ZipWriter<W>,
    node_modules_path: &Path,
    _parent_path: &Path,
    project_path: &Path,
    opts: zip::write::FileOptions<'static, ()>,
    progress: Option<&ProgressBar>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    let mut packages_to_bundle = std::collections::HashSet::new();

    let package_json_path = project_path.join("package.json");
    if let Ok(package_json_content) = fs::read_to_string(&package_json_path) {
        if let Ok(package_json) = serde_json::from_str::<Value>(&package_json_content) {
            if let Some(deps) = package_json["dependencies"].as_object() {
                for dep_name in deps.keys() {
                    packages_to_bundle.insert(dep_name.clone());
                }
            }
            if let Some(peer_deps) = package_json["peerDependencies"].as_object() {
                for dep_name in peer_deps.keys() {
                    packages_to_bundle.insert(dep_name.clone());
                }
            }
            if let Some(optional_deps) = package_json["optionalDependencies"].as_object() {
                for dep_name in optional_deps.keys() {
                    packages_to_bundle.insert(dep_name.clone());
                }
            }
        }
    }

    let mut resolved_packages = std::collections::HashSet::new();
    for package_name in &packages_to_bundle {
        resolve_workspace_dependencies(
            node_modules_path,
            package_name,
            &mut resolved_packages,
            0, // depth
        )?;
    }

    debug!(
        "Bundling {} packages (resolved dependencies) for workspace node_modules",
        resolved_packages.len()
    );

    zip.add_directory("app/node_modules/", opts)?;

    for package_name in &resolved_packages {
        if let Err(e) = copy_workspace_package(zip, node_modules_path, package_name, opts, progress)
        {
            warn!("Failed to copy package {package_name}: {e}");
        }
    }

    let bin_dir = node_modules_path.join(".bin");
    if bin_dir.exists() {
        if let Some(pb) = progress {
            pb.set_length(pb.length().unwrap_or(0) + count_files_in_dir(&bin_dir, false, false));
        }
        add_dir_to_zip_no_follow(
            zip,
            &bin_dir,
            Path::new("app/node_modules/.bin"),
            opts,
            progress,
        )?;
    }

    let important_files = [".modules.yaml"];
    for file_name in important_files {
        let file_path = node_modules_path.join(file_name);
        if file_path.exists() {
            let dest_path = Path::new("app/node_modules").join(file_name);
            zip.start_file(dest_path.to_string_lossy().as_ref(), opts)?;
            let data = fs::read(&file_path)?;
            zip.write_all(&data)?;
            if let Some(pb) = progress {
                pb.inc(1);
            }
        }
    }

    Ok(())
}

/// Bundle pnpm workspace dependencies (node_modules from parent)
fn bundle_pnpm_workspace_dependencies<W>(
    zip: &mut ZipWriter<W>,
    parent_path: &Path,
    project_path: &Path,
    opts: zip::write::FileOptions<'static, ()>,
    progress: Option<&ProgressBar>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    let mut packages_to_bundle = std::collections::HashSet::new();

    let package_json_path = project_path.join("package.json");
    if let Ok(package_json_content) = fs::read_to_string(&package_json_path) {
        if let Ok(package_json) = serde_json::from_str::<Value>(&package_json_content) {
            if let Some(deps) = package_json["dependencies"].as_object() {
                for dep_name in deps.keys() {
                    packages_to_bundle.insert(dep_name.clone());
                }
            }
            if let Some(peer_deps) = package_json["peerDependencies"].as_object() {
                for dep_name in peer_deps.keys() {
                    packages_to_bundle.insert(dep_name.clone());
                }
            }
            if let Some(optional_deps) = package_json["optionalDependencies"].as_object() {
                for dep_name in optional_deps.keys() {
                    packages_to_bundle.insert(dep_name.clone());
                }
            }
        }
    }

    let mut resolved_packages = std::collections::HashSet::new();
    for package_name in &packages_to_bundle {
        resolve_package_dependencies(
            &parent_path.join("node_modules"),
            &parent_path.join("node_modules").join(".pnpm"),
            package_name,
            &mut resolved_packages,
            0, // depth
        )?;
    }

    debug!(
        "Bundling {} packages (resolved dependencies) for workspace pnpm node_modules",
        resolved_packages.len()
    );

    // Ensure app/node_modules directory exists
    zip.add_directory("app/node_modules/", opts)?;

    for package_name in &resolved_packages {
        if let Err(e) = copy_pnpm_package_comprehensive(
            zip,
            &parent_path.join("node_modules"),
            &parent_path.join("node_modules").join(".pnpm"),
            package_name,
            opts,
            progress,
        ) {
            warn!("Failed to copy package {package_name}: {e}");
        }
    }

    let bin_dir = parent_path.join("node_modules").join(".bin");
    if bin_dir.exists() {
        if let Some(pb) = progress {
            pb.set_length(pb.length().unwrap_or(0) + count_files_in_dir(&bin_dir, false, false));
        }
        add_dir_to_zip_no_follow(
            zip,
            &bin_dir,
            Path::new("app/node_modules/.bin"),
            opts,
            progress,
        )?;
    }

    let important_files = [".modules.yaml", ".pnpm-workspace-state-v1.json"];
    for file_name in important_files {
        let file_path = parent_path.join("node_modules").join(file_name);
        if file_path.exists() {
            let dest_path = Path::new("app/node_modules").join(file_name);
            zip.start_file(dest_path.to_string_lossy().as_ref(), opts)?;
            let data = fs::read(&file_path)?;
            zip.write_all(&data)?;
            if let Some(pb) = progress {
                pb.inc(1);
            }
        }
    }

    Ok(())
}

/// Enhanced Node version detection with workspace support and version resolution.
async fn detect_node_version_with_workspace_support(
    project_path: &Path,
    ignore_cached_versions: bool,
) -> Result<String> {
    let version_manager = NodeVersionManager::new();
    let version_spec = find_node_version_spec(project_path)?;

    version_manager
        .resolve_version(&version_spec, ignore_cached_versions)
        .await
}

/// Find Node version specification from .nvmrc or .node-version files,
/// supporting workspace packages (parent/package, parent/packages/package patterns)
fn find_node_version_spec(project_path: &Path) -> Result<String> {
    let mut current_path = project_path;

    loop {
        for file in [".nvmrc", ".node-version"] {
            let version_file = current_path.join(file);
            if version_file.exists() {
                let content = fs::read_to_string(&version_file)
                    .with_context(|| format!("Failed to read {}", version_file.display()))?;
                let version_spec = content.trim();
                if !version_spec.is_empty() {
                    return Ok(normalize_node_version_spec(version_spec));
                }
            }
        }

        if is_workspace_root(current_path) || current_path.parent().is_none() {
            break;
        }

        current_path = current_path.parent().unwrap();
    }

    anyhow::bail!("Node version specification not found in project or workspace hierarchy")
}

/// Check if a directory is a workspace root (contains workspace configuration)
fn is_workspace_root(path: &Path) -> bool {
    let workspace_files = ["pnpm-workspace.yaml", "lerna.json", "rush.json", "nx.json"];

    for file in workspace_files {
        if path.join(file).exists() {
            return true;
        }
    }

    if let Ok(package_json_content) = fs::read_to_string(path.join("package.json")) {
        if let Ok(package_json) = serde_json::from_str::<serde_json::Value>(&package_json_content) {
            if package_json.get("workspaces").is_some() {
                return true;
            }
        }
    }

    false
}

/// Normalize a Node version specification (remove 'v' prefix, handle various formats)
fn normalize_node_version_spec(raw: &str) -> String {
    raw.trim().trim_start_matches('v').to_owned()
}

/// Determine the correct source directory to bundle for the project.
/// This handles TypeScript projects and other build configurations.
fn determine_source_directory(project_path: &Path, package_json: &Value) -> Result<PathBuf> {
    if let Some(main) = package_json["main"].as_str() {
        let main_path = project_path.join(main);
        if let Some(parent) = main_path.parent() {
            let parent_name = parent
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");

            if ["dist", "build", "lib", "out"].contains(&parent_name) && parent.exists() {
                return Ok(parent.to_path_buf());
            }
        }
    }

    let tsconfig_path = project_path.join("tsconfig.json");
    if tsconfig_path.exists() {
        if let Ok(tsconfig) = read_tsconfig(&tsconfig_path) {
            if let Some(out_dir) = tsconfig["compilerOptions"]["outDir"].as_str() {
                let out_path = project_path.join(out_dir);
                if out_path.exists() {
                    return Ok(out_path);
                }
            }
        }
    }

    for dir_name in ["dist", "build", "lib", "out"] {
        let dir_path = project_path.join(dir_name);
        if dir_path.exists()
            && dir_path.is_dir()
            && (contains_js_files(&dir_path) || dir_path.join("package.json").exists())
        {
            return Ok(dir_path);
        }
    }

    Ok(project_path.to_path_buf())
}

/// Read and parse tsconfig.json, handling extends configuration
fn read_tsconfig(tsconfig_path: &Path) -> Result<Value> {
    let content = fs::read_to_string(tsconfig_path).context("Failed to read tsconfig.json")?;

    let cleaned_content = content
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut config: Value =
        serde_json::from_str(&cleaned_content).context("Failed to parse tsconfig.json")?;

    if let Some(extends) = config["extends"].as_str() {
        let base_path = if extends.starts_with('.') {
            tsconfig_path.parent().unwrap().join(extends)
        } else {
            return Ok(config);
        };

        let base_path = if base_path.extension().is_none() {
            base_path.with_extension("json")
        } else {
            base_path
        };

        if base_path.exists() {
            if let Ok(base_config) = read_tsconfig(&base_path) {
                if let (Some(base_obj), Some(current_obj)) =
                    (base_config.as_object(), config.as_object())
                {
                    let mut merged = base_obj.clone();
                    for (key, value) in current_obj {
                        merged.insert(key.clone(), value.clone());
                    }
                    config = Value::Object(merged);
                }
            }
        }
    }

    Ok(config)
}

/// Check if a directory contains JavaScript files
fn contains_js_files(dir: &Path) -> bool {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".js") || name.ends_with(".mjs") || name.ends_with(".cjs") {
                    return true;
                }
            }
        }
    }
    false
}

/// Resolve the output path, handling naming conflicts
fn resolve_output_path(
    output_path: Option<PathBuf>,
    app_name: &str,
    custom_name: Option<&str>,
) -> Result<PathBuf> {
    if let Some(path) = output_path {
        return Ok(path);
    }

    let ext = if Platform::current().is_windows() {
        ".exe"
    } else {
        ""
    };
    let base_name = custom_name.unwrap_or(app_name);
    let mut output_path = PathBuf::from(format!("{base_name}{ext}"));

    let mut counter = 1;
    while output_path.exists() {
        if output_path.is_dir() {
            output_path = PathBuf::from(format!("{base_name}-bundle{ext}"));
            if !output_path.exists() {
                break;
            }
        }

        if output_path.exists() {
            output_path = PathBuf::from(format!("{base_name}-bundle-{counter}{ext}"));
            counter += 1;
        }
    }

    Ok(output_path)
}

// ────────────────────────────────────────────────────────────────────────────
// Self-extracting executable generation using a more reliable approach
// ────────────────────────────────────────────────────────────────────────────

// ────────────────────────────────────────────────────────────────────────────
// Utility helpers
// ────────────────────────────────────────────────────────────────────────────

fn add_dir_to_zip<W>(
    zip: &mut ZipWriter<W>,
    src_dir: &Path,
    dest_dir: &Path,
    opts: zip::write::FileOptions<'static, ()>,
    progress: Option<&ProgressBar>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    for entry in walkdir::WalkDir::new(src_dir).follow_links(true) {
        let entry = entry?;
        let path = entry.path();
        let rel_path = path.strip_prefix(src_dir).unwrap();
        let zip_path = dest_dir.join(rel_path);

        if entry.file_type().is_dir() {
            zip.add_directory(zip_path.to_string_lossy().as_ref(), opts)?;
            continue;
        }

        if !entry.file_type().is_file() && !entry.file_type().is_symlink() {
            continue;
        }

        let file_opts = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let metadata = fs::metadata(path)?;
                let permissions = metadata.permissions();
                let mode = permissions.mode();
                opts.unix_permissions(mode)
            }
            #[cfg(not(unix))]
            {
                opts
            }
        };

        zip.start_file(zip_path.to_string_lossy().as_ref(), file_opts)?;
        let data = fs::read(path).context("Failed to read file while zipping")?;
        zip.write_all(&data)?;
        if let Some(pb) = progress {
            pb.inc(1);
        }
    }
    Ok(())
}

/// Add directory to zip without following symlinks but preserving them
fn add_dir_to_zip_no_follow<W>(
    zip: &mut ZipWriter<W>,
    src_dir: &Path,
    dest_dir: &Path,
    opts: zip::write::FileOptions<'static, ()>,
    progress: Option<&ProgressBar>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    for entry in walkdir::WalkDir::new(src_dir).follow_links(false) {
        let entry = entry?;
        let path = entry.path();
        let rel_path = path.strip_prefix(src_dir).unwrap();
        let zip_path = dest_dir.join(rel_path);

        if entry.file_type().is_dir() {
            zip.add_directory(zip_path.to_string_lossy().as_ref(), opts)?;
            continue;
        }

        if !entry.file_type().is_file() && !entry.file_type().is_symlink() {
            continue;
        }

        let file_opts = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let metadata = entry.metadata()?;
                let permissions = metadata.permissions();
                let mode = permissions.mode();
                opts.unix_permissions(mode)
            }
            #[cfg(not(unix))]
            {
                opts
            }
        };

        zip.start_file(zip_path.to_string_lossy().as_ref(), file_opts)?;

        if entry.file_type().is_symlink() {
            if let Ok(target) = fs::read_link(path) {
                let target_str = target.to_string_lossy();
                zip.write_all(target_str.as_bytes())?;
            }
        } else {
            let data = fs::read(path).context("Failed to read file while zipping")?;
            zip.write_all(&data)?;
        }
        if let Some(pb) = progress {
            pb.inc(1);
        }
    }
    Ok(())
}

/// Add directory to zip without following symlinks and skipping parent directory creation
fn add_dir_to_zip_no_follow_skip_parents<W>(
    zip: &mut ZipWriter<W>,
    src_dir: &Path,
    dest_dir: &Path,
    opts: zip::write::FileOptions<'static, ()>,
    progress: Option<&ProgressBar>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    for entry in walkdir::WalkDir::new(src_dir).follow_links(false) {
        let entry = entry?;
        let path = entry.path();
        let rel_path = path.strip_prefix(src_dir).unwrap();
        let zip_path = dest_dir.join(rel_path);

        if entry.file_type().is_dir() {
            if !rel_path.as_os_str().is_empty() {
                zip.add_directory(zip_path.to_string_lossy().as_ref(), opts)?;
            }
            continue;
        }

        if !entry.file_type().is_file() && !entry.file_type().is_symlink() {
            continue;
        }

        let file_opts = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let metadata = entry.metadata()?;
                let permissions = metadata.permissions();
                let mode = permissions.mode();
                opts.unix_permissions(mode)
            }
            #[cfg(not(unix))]
            {
                opts
            }
        };

        zip.start_file(zip_path.to_string_lossy().as_ref(), file_opts)?;

        if entry.file_type().is_symlink() {
            if let Ok(target) = fs::read_link(path) {
                let target_str = target.to_string_lossy();
                zip.write_all(target_str.as_bytes())?;
            }
        } else {
            let data = fs::read(path).context("Failed to read file while zipping")?;
            zip.write_all(&data)?;
        }
        if let Some(pb) = progress {
            pb.inc(1);
        }
    }
    Ok(())
}

/// Add directory to zip, excluding node_modules from the source directory
fn add_dir_to_zip_excluding_node_modules<W>(
    zip: &mut ZipWriter<W>,
    src_dir: &Path,
    dest_dir: &Path,
    opts: zip::write::FileOptions<'static, ()>,
    progress: Option<&ProgressBar>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    for entry in walkdir::WalkDir::new(src_dir).follow_links(true) {
        let entry = entry?;
        let path = entry.path();
        let rel_path = path.strip_prefix(src_dir).unwrap();
        let zip_path = dest_dir.join(rel_path);

        if rel_path.starts_with("node_modules") {
            continue;
        }

        if entry.file_type().is_dir() {
            zip.add_directory(zip_path.to_string_lossy().as_ref(), opts)?;
            continue;
        }

        if !entry.file_type().is_file() && !entry.file_type().is_symlink() {
            continue;
        }

        let file_opts = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let metadata = fs::metadata(path)?;
                let permissions = metadata.permissions();
                let mode = permissions.mode();
                opts.unix_permissions(mode)
            }
            #[cfg(not(unix))]
            {
                opts
            }
        };

        zip.start_file(zip_path.to_string_lossy().as_ref(), file_opts)?;
        let data = fs::read(path).context("Failed to read file while zipping")?;
        zip.write_all(&data)?;
        if let Some(pb) = progress {
            pb.inc(1);
        }
    }
    Ok(())
}

/// Copy a package from workspace node_modules (for regular npm/yarn workspaces)
fn copy_workspace_package<W>(
    zip: &mut ZipWriter<W>,
    node_modules_path: &Path,
    package_name: &str,
    opts: zip::write::FileOptions<'static, ()>,
    progress: Option<&ProgressBar>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    let dest_path = Path::new("app/node_modules").join(package_name);
    let package_path = node_modules_path.join(package_name);

    if package_path.exists() {
        let target_path = if package_path.is_symlink() {
            // Follow the symlink
            let target = fs::read_link(&package_path)?;
            if target.is_absolute() {
                target
            } else {
                package_path.parent().unwrap().join(target).canonicalize()?
            }
        } else {
            package_path
        };

        if target_path.exists() {
            if let Some(pb) = progress {
                pb.set_length(
                    pb.length().unwrap_or(0) + count_files_in_dir(&target_path, false, false),
                );
            }
            add_dir_to_zip_no_follow_skip_parents(zip, &target_path, &dest_path, opts, progress)?;
            return Ok(());
        }
    }

    anyhow::bail!(
        "Package {} not found in workspace node_modules",
        package_name
    )
}

/// Resolve dependencies for regular workspaces (non-pnpm)
fn resolve_workspace_dependencies(
    node_modules_path: &Path,
    package_name: &str,
    resolved: &mut std::collections::HashSet<String>,
    depth: usize,
) -> Result<()> {
    // Avoid infinite recursion
    if depth > 20 {
        return Ok(());
    }

    if resolved.contains(package_name) {
        return Ok(());
    }

    resolved.insert(package_name.to_string());

    let package_path = node_modules_path.join(package_name);
    let package_json_path = if package_path.is_symlink() {
        let target = fs::read_link(&package_path)?;
        let target_path = if target.is_absolute() {
            target
        } else {
            package_path.parent().unwrap().join(target).canonicalize()?
        };
        target_path.join("package.json")
    } else {
        package_path.join("package.json")
    };

    if !package_json_path.exists() {
        return Ok(()); // Skip packages we can't find
    }

    let package_json_content =
        fs::read_to_string(&package_json_path).context("Failed to read package.json")?;

    if let Ok(package_json) = serde_json::from_str::<Value>(&package_json_content) {
        if let Some(deps) = package_json["dependencies"].as_object() {
            for dep_name in deps.keys() {
                resolve_workspace_dependencies(node_modules_path, dep_name, resolved, depth + 1)?;
            }
        }

        if let Some(peer_deps) = package_json["peerDependencies"].as_object() {
            for dep_name in peer_deps.keys() {
                let dep_path = node_modules_path.join(dep_name);
                if dep_path.exists() {
                    resolve_workspace_dependencies(
                        node_modules_path,
                        dep_name,
                        resolved,
                        depth + 1,
                    )?;
                }
            }
        }

        if let Some(optional_deps) = package_json["optionalDependencies"].as_object() {
            for dep_name in optional_deps.keys() {
                let dep_path = node_modules_path.join(dep_name);
                if dep_path.exists() {
                    resolve_workspace_dependencies(
                        node_modules_path,
                        dep_name,
                        resolved,
                        depth + 1,
                    )?;
                }
            }
        }
    }

    Ok(())
}
