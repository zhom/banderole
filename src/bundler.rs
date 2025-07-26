use crate::node_downloader::NodeDownloader;
use crate::platform::Platform;
use anyhow::{Context, Result};
use base64::Engine as _;
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;
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
) -> Result<()> {
    // 1. Validate & canonicalize input directory.
    let project_path = project_path
        .canonicalize()
        .context("Failed to resolve project path")?;
    let pkg_json = project_path.join("package.json");
    anyhow::ensure!(
        pkg_json.exists(),
        "package.json not found in {}",
        project_path.display()
    );

    // 2. Read `package.json` for name/version and detect project structure
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

    // 3. Determine the correct source directory to bundle
    let source_dir = determine_source_directory(&project_path, &package_value)?;

    // 4. Determine Node version (via .nvmrc / .node-version or default to LTS 22.17.1).
    let node_version = detect_node_version(&project_path).unwrap_or_else(|_| "22.17.1".into());

    println!(
        "Bundling {app_name} v{app_version} using Node.js v{node_version} for {plat}",
        plat = Platform::current()
    );

    if source_dir != project_path {
        println!("Using source directory: {}", source_dir.display());
    }

    // 5. Resolve output path with collision handling
    let output_path = resolve_output_path(output_path, &app_name, custom_name.as_deref())?;

    // 6. Ensure portable Node binary is available.
    let node_downloader = NodeDownloader::new_with_persistent_cache(node_version.clone())?;
    let node_executable = node_downloader.ensure_node_binary().await?;
    let node_root = node_executable
        .parent()
        .expect("node executable must have a parent")
        .parent()
        .unwrap_or_else(|| panic!("Unexpected node layout for {}", node_executable.display()));

    // 7. Create an in-memory zip archive containing `/app` and `/node` directories.
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

        // Copy the determined source directory (excluding node_modules to avoid conflicts)
        add_dir_to_zip_excluding_node_modules(&mut zip, &source_dir, Path::new("app"), opts)?;

        // Handle dependencies and package.json
        bundle_dependencies(&mut zip, &project_path, &source_dir, &package_value, opts)?;

        // Copy Node runtime directory.
        add_dir_to_zip(&mut zip, node_root, Path::new("node"), opts)?;
        zip.finish()?;
    }

    // 8. Build self-extracting launcher using a more reliable approach.
    create_self_extracting_executable(&output_path, zip_data, &app_name)?;

    println!("Bundle created at {}", output_path.display());
    Ok(())
}

/// Bundle dependencies with improved package manager support
fn bundle_dependencies<W>(
    zip: &mut ZipWriter<W>,
    project_path: &Path,
    source_dir: &Path,
    _package_value: &Value,
    opts: zip::write::FileOptions<'static, ()>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    // If we're using a subdirectory, copy the root package.json with adjusted paths
    if source_dir != project_path {
        let root_package_json = project_path.join("package.json");
        if root_package_json.exists() {
            zip.start_file("app/package.json", opts)?;

            // Read and modify package.json to adjust the main path
            let content = fs::read_to_string(&root_package_json)
                .context("Failed to read root package.json")?;
            let mut package_value: Value =
                serde_json::from_str(&content).context("Failed to parse root package.json")?;

            // Adjust the main field if it points to the source directory
            if let Some(main) = package_value["main"].as_str() {
                let main_path = project_path.join(main);
                if let Ok(relative_to_source) = main_path.strip_prefix(&source_dir) {
                    package_value["main"] =
                        Value::String(relative_to_source.to_string_lossy().to_string());
                }
            }

            let modified_content = serde_json::to_string_pretty(&package_value)
                .context("Failed to serialize modified package.json")?;
            zip.write_all(modified_content.as_bytes())?;
        }
    }

    // Find and bundle dependencies with improved package manager support
    let deps_result = find_and_bundle_dependencies(zip, project_path, opts)?;

    // Log the result
    if deps_result.dependencies_found {
        println!("Bundled dependencies: {}", deps_result.source_description);
    } else {
        println!("Warning: No dependencies found to bundle");
    }

    // Log any warnings
    for warning in &deps_result.warnings {
        println!("Warning: {}", warning);
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
) -> Result<DependenciesResult>
where
    W: Write + Read + std::io::Seek,
{
    let mut warnings = Vec::new();

    // Strategy 1: Check for node_modules in the project directory
    let project_node_modules = project_path.join("node_modules");
    if project_node_modules.exists() {
        let package_manager = detect_package_manager(&project_node_modules, project_path);

        // Check if this is a pnpm workspace (symlinks to parent .pnpm)
        let is_pnpm_workspace = if package_manager == PackageManager::Pnpm {
            // Check if the pnpm structure points to a parent directory
            if let Ok(entries) = fs::read_dir(&project_node_modules) {
                entries.flatten().any(|entry| {
                    if entry.file_type().ok().map_or(false, |ft| ft.is_symlink()) {
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

        // If it's a pnpm workspace, skip local bundling and go to workspace detection
        if !is_pnpm_workspace {
            match package_manager {
                PackageManager::Pnpm => {
                    // For local pnpm, bundle both node_modules and .pnpm if it exists
                    bundle_pnpm_dependencies(zip, project_path, opts)?;
                    return Ok(DependenciesResult {
                        dependencies_found: true,
                        source_description: "pnpm dependencies (node_modules + .pnpm)".to_string(),
                        warnings,
                    });
                }
                PackageManager::Yarn => {
                    // For yarn, bundle node_modules with comprehensive dependency resolution
                    bundle_node_modules_comprehensive(
                        zip,
                        &project_node_modules,
                        project_path,
                        opts,
                    )?;
                    return Ok(DependenciesResult {
                        dependencies_found: true,
                        source_description: "yarn dependencies (node_modules)".to_string(),
                        warnings,
                    });
                }
                PackageManager::Npm | PackageManager::Unknown => {
                    // For npm or unknown, use comprehensive bundling
                    bundle_node_modules_comprehensive(
                        zip,
                        &project_node_modules,
                        project_path,
                        opts,
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
            // Check if this is a workspace root
            let mut is_workspace = false;

            // Check package.json for workspace configuration
            if let Ok(content) = fs::read_to_string(&parent_package_json) {
                if let Ok(pkg_value) = serde_json::from_str::<Value>(&content) {
                    is_workspace = pkg_value["workspaces"].is_array()
                        || pkg_value["workspaces"]["packages"].is_array()
                        || pkg_value["workspaces"].is_object();
                }
            }

            // Check for pnpm-workspace.yaml
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
                        bundle_pnpm_workspace_dependencies(zip, parent_path, project_path, opts)?;
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

        // Don't go too far up the directory tree
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
    // Check for pnpm-specific structure
    if node_modules_path.join(".pnpm").exists() {
        return PackageManager::Pnpm;
    }

    // Check if this is a pnpm workspace (symlinks pointing to parent .pnpm)
    if node_modules_path.exists() {
        if let Ok(entries) = fs::read_dir(node_modules_path) {
            for entry in entries.flatten() {
                if entry.file_type().ok().map_or(false, |ft| ft.is_symlink()) {
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

    // Check for lockfiles in the project directory
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
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    let node_modules_path = project_path.join("node_modules");
    let pnpm_dir = node_modules_path.join(".pnpm");

    if !pnpm_dir.exists() {
        // If no .pnpm directory, fall back to simple copy
        if node_modules_path.exists() {
            add_dir_to_zip_no_follow(zip, &node_modules_path, Path::new("app/node_modules"), opts)?;
        }
        return Ok(());
    }

    // For pnpm, use a smarter approach that only includes actually needed packages
    let mut packages_to_bundle = std::collections::HashSet::new();

    // Start with direct dependencies from package.json
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

    // Recursively resolve dependencies for each package
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

    println!(
        "Bundling {} packages (resolved dependencies) for pnpm project",
        resolved_packages.len()
    );

    // Ensure app/node_modules directory exists
    zip.add_directory("app/node_modules/", opts)?;

    // Copy each resolved package
    for package_name in &resolved_packages {
        if let Err(e) =
            copy_pnpm_package_comprehensive(zip, &node_modules_path, &pnpm_dir, package_name, opts)
        {
            println!("Warning: Failed to copy package {}: {}", package_name, e);
        }
    }

    // Copy .bin directory if it exists
    let bin_dir = node_modules_path.join(".bin");
    if bin_dir.exists() {
        add_dir_to_zip_no_follow(zip, &bin_dir, Path::new("app/node_modules/.bin"), opts)?;
    }

    // Copy important pnpm metadata files
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

    // If already resolved, skip
    if resolved.contains(package_name) {
        return Ok(());
    }

    resolved.insert(package_name.to_string());

    // Try to find the package and read its package.json
    let package_json_content =
        match find_package_json_content(node_modules_path, pnpm_dir, package_name) {
            Ok(content) => content,
            Err(_) => return Ok(()), // Skip packages we can't find
        };

    if let Ok(package_json) = serde_json::from_str::<Value>(&package_json_content) {
        // Add production dependencies
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

        // Also include peerDependencies that are actually installed
        if let Some(peer_deps) = package_json["peerDependencies"].as_object() {
            for dep_name in peer_deps.keys() {
                // Only include if it actually exists
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

        // Also include optionalDependencies that are actually installed (important for native bindings)
        if let Some(optional_deps) = package_json["optionalDependencies"].as_object() {
            for dep_name in optional_deps.keys() {
                // Only include if it actually exists
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
    // First try top-level
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

    // Try .pnpm directory
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
    // Check top-level
    if node_modules_path.join(package_name).exists() {
        return true;
    }

    // Check .pnpm
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
    // Handle scoped packages like "@sindresorhus+is@4.6.0" -> "@sindresorhus/is"
    if pnpm_name.starts_with('@') {
        if let Some(at_pos) = pnpm_name.rfind('@') {
            if at_pos > 0 {
                // Make sure it's not the first @
                let package_part = &pnpm_name[..at_pos];
                // Convert + back to / for scoped packages
                return Some(package_part.replace('+', "/"));
            }
        }
        // If no version found, just convert + to /
        return Some(pnpm_name.replace('+', "/"));
    }

    // Handle regular packages like "adm-zip@0.5.16"
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
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    let dest_path = Path::new("app/node_modules").join(package_name);

    // First try to find it as a top-level package
    let top_level_package = node_modules_path.join(package_name);
    if top_level_package.exists() {
        let target_path = if top_level_package.is_symlink() {
            // Follow the symlink
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
            add_dir_to_zip_no_follow_skip_parents(zip, &target_path, &dest_path, opts)?;
            return Ok(());
        }
    }

    // If not found at top level, search in .pnpm directory
    for entry in fs::read_dir(pnpm_dir)? {
        let entry = entry?;
        let pnpm_package_name = entry.file_name().to_string_lossy().to_string();

        // Check if this .pnpm entry matches our package name
        if let Some(extracted_name) = extract_package_name_from_pnpm(&pnpm_package_name) {
            if extracted_name == package_name {
                let pnpm_package_path = entry.path().join("node_modules").join(package_name);
                if pnpm_package_path.exists() {
                    add_dir_to_zip_no_follow_skip_parents(
                        zip,
                        &pnpm_package_path,
                        &dest_path,
                        opts,
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
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    let mut packages_to_bundle = std::collections::HashSet::new();

    // Start with direct dependencies from package.json
    let package_json_path = project_path.join("package.json");
    if let Ok(package_json_content) = fs::read_to_string(&package_json_path) {
        if let Ok(package_json) = serde_json::from_str::<Value>(&package_json_content) {
            if let Some(deps) = package_json["dependencies"].as_object() {
                for dep_name in deps.keys() {
                    packages_to_bundle.insert(dep_name.clone());
                }
            }
            // Also include peerDependencies and optionalDependencies
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

    // Check if this is a pnpm setup
    let pnpm_dir = node_modules_path.join(".pnpm");
    if pnpm_dir.exists() {
        // Use pnpm-specific resolution
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

        println!(
            "Bundling {} packages (resolved dependencies) for pnpm node_modules",
            resolved_packages.len()
        );

        // Ensure app/node_modules directory exists
        zip.add_directory("app/node_modules/", opts)?;

        // Copy each resolved package using pnpm logic
        for package_name in &resolved_packages {
            if let Err(e) = copy_pnpm_package_comprehensive(
                zip,
                node_modules_path,
                &pnpm_dir,
                package_name,
                opts,
            ) {
                println!("Warning: Failed to copy package {}: {}", package_name, e);
            }
        }
    } else {
        // Use regular workspace resolution for non-pnpm setups
        let mut resolved_packages = std::collections::HashSet::new();
        for package_name in &packages_to_bundle {
            resolve_workspace_dependencies(
                node_modules_path,
                package_name,
                &mut resolved_packages,
                0,
            )?;
        }

        println!(
            "Bundling {} packages (resolved dependencies) for regular node_modules",
            resolved_packages.len()
        );

        // Ensure app/node_modules directory exists
        zip.add_directory("app/node_modules/", opts)?;

        // Copy each resolved package using workspace logic
        for package_name in &resolved_packages {
            if let Err(e) = copy_workspace_package(zip, node_modules_path, package_name, opts) {
                println!("Warning: Failed to copy package {}: {}", package_name, e);
            }
        }
    }

    // Copy .bin directory if it exists
    let bin_dir = node_modules_path.join(".bin");
    if bin_dir.exists() {
        add_dir_to_zip_no_follow(zip, &bin_dir, Path::new("app/node_modules/.bin"), opts)?;
    }

    // Copy important metadata files
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

/// Bundle workspace dependencies (node_modules from parent)
fn bundle_workspace_dependencies<W>(
    zip: &mut ZipWriter<W>,
    node_modules_path: &Path,
    _parent_path: &Path,
    project_path: &Path,
    opts: zip::write::FileOptions<'static, ()>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    let mut packages_to_bundle = std::collections::HashSet::new();

    // Read dependencies from the ACTUAL PROJECT being bundled, not the workspace root
    let package_json_path = project_path.join("package.json");
    if let Ok(package_json_content) = fs::read_to_string(&package_json_path) {
        if let Ok(package_json) = serde_json::from_str::<Value>(&package_json_content) {
            if let Some(deps) = package_json["dependencies"].as_object() {
                for dep_name in deps.keys() {
                    packages_to_bundle.insert(dep_name.clone());
                }
            }
            // Also include peerDependencies and optionalDependencies
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

    // Recursively resolve dependencies for each package using workspace-specific logic
    let mut resolved_packages = std::collections::HashSet::new();
    for package_name in &packages_to_bundle {
        resolve_workspace_dependencies(
            node_modules_path,
            package_name,
            &mut resolved_packages,
            0, // depth
        )?;
    }

    println!(
        "Bundling {} packages (resolved dependencies) for workspace node_modules",
        resolved_packages.len()
    );

    // Ensure app/node_modules directory exists
    zip.add_directory("app/node_modules/", opts)?;

    // Copy each resolved package using workspace-specific copying
    for package_name in &resolved_packages {
        if let Err(e) = copy_workspace_package(zip, node_modules_path, package_name, opts) {
            println!("Warning: Failed to copy package {}: {}", package_name, e);
        }
    }

    // Copy .bin directory if it exists
    let bin_dir = node_modules_path.join(".bin");
    if bin_dir.exists() {
        add_dir_to_zip_no_follow(zip, &bin_dir, Path::new("app/node_modules/.bin"), opts)?;
    }

    // Copy important workspace metadata files if they exist
    let important_files = [".modules.yaml"];
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

/// Bundle pnpm workspace dependencies (node_modules from parent)
fn bundle_pnpm_workspace_dependencies<W>(
    zip: &mut ZipWriter<W>,
    parent_path: &Path,
    project_path: &Path,
    opts: zip::write::FileOptions<'static, ()>,
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    let mut packages_to_bundle = std::collections::HashSet::new();

    // Read dependencies from the ACTUAL PROJECT being bundled, not the workspace root
    let package_json_path = project_path.join("package.json");
    if let Ok(package_json_content) = fs::read_to_string(&package_json_path) {
        if let Ok(package_json) = serde_json::from_str::<Value>(&package_json_content) {
            if let Some(deps) = package_json["dependencies"].as_object() {
                for dep_name in deps.keys() {
                    packages_to_bundle.insert(dep_name.clone());
                }
            }
            // Also include peerDependencies and optionalDependencies
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

    // Recursively resolve dependencies for each package using pnpm-specific logic
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

    println!(
        "Bundling {} packages (resolved dependencies) for workspace pnpm node_modules",
        resolved_packages.len()
    );

    // Ensure app/node_modules directory exists
    zip.add_directory("app/node_modules/", opts)?;

    // Copy each resolved package using pnpm-specific copying
    for package_name in &resolved_packages {
        if let Err(e) = copy_pnpm_package_comprehensive(
            zip,
            &parent_path.join("node_modules"),
            &parent_path.join("node_modules").join(".pnpm"),
            package_name,
            opts,
        ) {
            println!("Warning: Failed to copy package {}: {}", package_name, e);
        }
    }

    // Copy .bin directory if it exists
    let bin_dir = parent_path.join("node_modules").join(".bin");
    if bin_dir.exists() {
        add_dir_to_zip_no_follow(zip, &bin_dir, Path::new("app/node_modules/.bin"), opts)?;
    }

    // Copy important pnpm metadata files
    let important_files = [".modules.yaml", ".pnpm-workspace-state-v1.json"];
    for file_name in important_files {
        let file_path = parent_path.join("node_modules").join(file_name);
        if file_path.exists() {
            let dest_path = Path::new("app/node_modules").join(file_name);
            zip.start_file(dest_path.to_string_lossy().as_ref(), opts)?;
            let data = fs::read(&file_path)?;
            zip.write_all(&data)?;
        }
    }

    Ok(())
}

/// Very lightweight Node version detection.
fn detect_node_version(project_path: &Path) -> Result<String> {
    for file in [".nvmrc", ".node-version"] {
        let path = project_path.join(file);
        if path.exists() {
            let v = fs::read_to_string(&path)?;
            return Ok(normalise_node_version(v.trim()));
        }
    }
    anyhow::bail!("Node version not found")
}

fn normalise_node_version(raw: &str) -> String {
    raw.trim_start_matches('v').to_owned()
}

/// Determine the correct source directory to bundle for the project.
/// This handles TypeScript projects and other build configurations.
fn determine_source_directory(project_path: &Path, package_json: &Value) -> Result<PathBuf> {
    // Check if there's a specific main entry point that indicates a built project
    if let Some(main) = package_json["main"].as_str() {
        let main_path = project_path.join(main);
        if let Some(parent) = main_path.parent() {
            // If main points to a file in a subdirectory like dist/index.js or build/index.js
            let parent_name = parent
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");

            if ["dist", "build", "lib", "out"].contains(&parent_name) && parent.exists() {
                return Ok(parent.to_path_buf());
            }
        }
    }

    // Check for TypeScript configuration
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

    // Check for common build output directories
    for dir_name in ["dist", "build", "lib", "out"] {
        let dir_path = project_path.join(dir_name);
        if dir_path.exists() && dir_path.is_dir() {
            // Verify it contains JavaScript files or a package.json
            if contains_js_files(&dir_path) || dir_path.join("package.json").exists() {
                return Ok(dir_path);
            }
        }
    }

    // Default to the project root
    Ok(project_path.to_path_buf())
}

/// Read and parse tsconfig.json, handling extends configuration
fn read_tsconfig(tsconfig_path: &Path) -> Result<Value> {
    let content = fs::read_to_string(tsconfig_path).context("Failed to read tsconfig.json")?;

    // Remove comments for JSON parsing (simple approach)
    let cleaned_content = content
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut config: Value =
        serde_json::from_str(&cleaned_content).context("Failed to parse tsconfig.json")?;

    // Handle extends configuration
    if let Some(extends) = config["extends"].as_str() {
        let base_path = if extends.starts_with('.') {
            tsconfig_path.parent().unwrap().join(extends)
        } else {
            // Could be a package reference, but for now we'll skip
            return Ok(config);
        };

        // Add .json extension if not present
        let base_path = if base_path.extension().is_none() {
            base_path.with_extension("json")
        } else {
            base_path
        };

        if base_path.exists() {
            if let Ok(base_config) = read_tsconfig(&base_path) {
                // Merge base config with current config (simple merge)
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
        // If explicit output path is provided, use it as-is
        return Ok(path);
    }

    let ext = if Platform::current().is_windows() {
        ".exe"
    } else {
        ""
    };
    let base_name = custom_name.unwrap_or(app_name);
    let mut output_path = PathBuf::from(format!("{base_name}{ext}"));

    // Check for conflicts and resolve them
    let mut counter = 1;
    while output_path.exists() {
        if output_path.is_dir() {
            // If it's a directory, append a suffix
            output_path = PathBuf::from(format!("{base_name}-bundle{ext}"));
            if !output_path.exists() {
                break;
            }
        }

        // If it still exists (file or another directory), add a counter
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

fn create_self_extracting_executable(out: &Path, zip_data: Vec<u8>, _app_name: &str) -> Result<()> {
    let build_id = Uuid::new_v4();

    if Platform::current().is_windows() {
        create_windows_executable(out, zip_data, &build_id.to_string())
    } else {
        create_unix_executable(out, zip_data, &build_id.to_string())
    }
}

fn create_unix_executable(out: &Path, zip_data: Vec<u8>, build_id: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut file = fs::File::create(out).context("Failed to create output executable")?;

    // Write a shell script with improved queue system for concurrent execution
    let script = format!(
        r#"#!/bin/bash
set -e

# Determine cache directory using directories pattern
if [ -n "$XDG_CACHE_HOME" ]; then
    CACHE_DIR="$XDG_CACHE_HOME/banderole"
elif [ -n "$HOME" ]; then
    CACHE_DIR="$HOME/.cache/banderole"
else
    CACHE_DIR="/tmp/banderole-cache"
fi

APP_DIR="$CACHE_DIR/{}"
LOCK_FILE="$APP_DIR/.lock"
READY_FILE="$APP_DIR/.ready"
QUEUE_DIR="$APP_DIR/.queue"
EXTRACTION_PID_FILE="$APP_DIR/.extraction.pid"

# Function to run the application
run_app() {{
    cd "$APP_DIR/app"
    
    # Find main script from package.json
    MAIN_SCRIPT=$("$APP_DIR/node/bin/node" -e "try {{ console.log(require('./package.json').main || 'index.js'); }} catch(e) {{ console.log('index.js'); }}" 2>/dev/null || echo "index.js")
    
    if [ -f "$MAIN_SCRIPT" ]; then
        exec "$APP_DIR/node/bin/node" "$MAIN_SCRIPT" "$@"
    else
        echo "Error: Main script '$MAIN_SCRIPT' not found" >&2
        exit 1
    fi
}}

# Function to clean up queue entry
cleanup_queue() {{
    if [ -n "$QUEUE_ENTRY" ] && [ -f "$QUEUE_ENTRY" ]; then
        rm -f "$QUEUE_ENTRY" 2>/dev/null || true
    fi
}}

# Set up cleanup trap
trap cleanup_queue EXIT

# Check if already extracted and ready
if [ -f "$READY_FILE" ] && [ -f "$APP_DIR/app/package.json" ] && [ -x "$APP_DIR/node/bin/node" ]; then
    # Already extracted, run directly
    run_app "$@"
fi

# Create queue directory if it doesn't exist
mkdir -p "$QUEUE_DIR" 2>/dev/null || true

# Generate unique queue entry
QUEUE_ENTRY="$QUEUE_DIR/$$-$(date +%s%N)"
echo "$$" > "$QUEUE_ENTRY"

# Try to acquire extraction lock
(
    # Use flock if available, otherwise use mkdir as fallback
    if command -v flock >/dev/null 2>&1; then
        exec 200>"$LOCK_FILE"
        if ! flock -n 200; then
            # Another process is extracting, wait in queue
            while [ ! -f "$READY_FILE" ]; do
                # Check if extraction process is still alive
                if [ -f "$EXTRACTION_PID_FILE" ]; then
                    EXTRACTION_PID=$(cat "$EXTRACTION_PID_FILE" 2>/dev/null || echo "")
                    if [ -n "$EXTRACTION_PID" ] && ! kill -0 "$EXTRACTION_PID" 2>/dev/null; then
                        # Extraction process died, clean up and try again
                        rm -f "$EXTRACTION_PID_FILE" "$LOCK_FILE" 2>/dev/null || true
                        break
                    fi
                fi
                sleep 0.1
            done
            
            # Wait for our turn in the queue
            while [ -f "$QUEUE_ENTRY" ]; do
                if [ -f "$READY_FILE" ]; then
                    break
                fi
                
                # Check if we're the first in queue
                FIRST_QUEUE=$(ls -1 "$QUEUE_DIR" 2>/dev/null | head -n1 || echo "")
                if [ "$(basename "$QUEUE_ENTRY")" = "$FIRST_QUEUE" ]; then
                    break
                fi
                sleep 0.05
            done
            
            run_app "$@"
        fi
        
        # We got the lock, record our PID for extraction
        echo "$$" > "$EXTRACTION_PID_FILE"
    else
        # Fallback to mkdir-based locking
        while ! mkdir "$LOCK_FILE" 2>/dev/null; do
            if [ -f "$READY_FILE" ]; then
                # Wait for our turn in the queue
                while [ -f "$QUEUE_ENTRY" ]; do
                    if [ -f "$READY_FILE" ]; then
                        break
                    fi
                    
                    # Check if we're the first in queue
                    FIRST_QUEUE=$(ls -1 "$QUEUE_DIR" 2>/dev/null | head -n1 || echo "")
                    if [ "$(basename "$QUEUE_ENTRY")" = "$FIRST_QUEUE" ]; then
                        break
                    fi
                    sleep 0.05
                done
                
                run_app "$@"
            fi
            sleep 0.1
        done
        trap "rmdir '$LOCK_FILE' 2>/dev/null || true; rm -f '$EXTRACTION_PID_FILE' 2>/dev/null || true" EXIT
        
        # We got the lock, record our PID for extraction
        echo "$$" > "$EXTRACTION_PID_FILE"
    fi

    # Check again if extraction completed while we were waiting for lock
    if [ -f "$READY_FILE" ] && [ -f "$APP_DIR/app/package.json" ] && [ -x "$APP_DIR/node/bin/node" ]; then
        run_app "$@"
    fi

    # Extract application
    mkdir -p "$APP_DIR"

    # Create a temporary file for the zip data
    TEMP_ZIP=$(mktemp)
    trap "rm -f '$TEMP_ZIP'" EXIT

    # Extract embedded zip data (everything after the __DATA__ marker)
    awk '/^__DATA__$/{{p=1;next}} p{{print}}' "$0" | base64 -d > "$TEMP_ZIP"

    # Verify we got valid zip data
    if [ ! -s "$TEMP_ZIP" ]; then
        echo "Error: Failed to extract bundle data" >&2
        rm -rf "$APP_DIR"
        exit 1
    fi

    # Extract the bundle
    if ! unzip -q "$TEMP_ZIP" -d "$APP_DIR" 2>/dev/null; then
        echo "Error: Failed to extract bundle" >&2
        rm -rf "$APP_DIR"
        exit 1
    fi

    # Verify extraction worked
    if [ ! -f "$APP_DIR/app/package.json" ] || [ ! -x "$APP_DIR/node/bin/node" ]; then
        echo "Error: Bundle extraction incomplete" >&2
        rm -rf "$APP_DIR"
        exit 1
    fi

    # Mark as ready for other processes
    touch "$READY_FILE"

    # Process queue in order - wake up waiting processes
    if [ -d "$QUEUE_DIR" ]; then
        for queue_file in $(ls -1 "$QUEUE_DIR" 2>/dev/null | sort); do
            queue_path="$QUEUE_DIR/$queue_file"
            if [ -f "$queue_path" ]; then
                queue_pid=$(cat "$queue_path" 2>/dev/null || echo "")
                if [ -n "$queue_pid" ] && kill -0 "$queue_pid" 2>/dev/null; then
                    # Signal the waiting process by removing its queue file
                    rm -f "$queue_path" 2>/dev/null || true
                fi
            fi
        done
    fi

    # Clean up extraction metadata
    rm -f "$EXTRACTION_PID_FILE" 2>/dev/null || true
    
    # Clean up lock
    if command -v flock >/dev/null 2>&1; then
        exec 200>&-
    else
        rmdir "$LOCK_FILE" 2>/dev/null || true
    fi
)

# Run the application
run_app "$@"

__DATA__
"#,
        build_id
    );

    file.write_all(script.as_bytes())?;

    // Append base64-encoded zip data
    let encoded = base64::engine::general_purpose::STANDARD.encode(&zip_data);
    file.write_all(encoded.as_bytes())?;
    file.write_all(b"\n")?;

    // Make executable
    let mut perms = file.metadata()?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(out, perms)?;

    Ok(())
}

fn create_windows_executable(out: &Path, zip_data: Vec<u8>, build_id: &str) -> Result<()> {
    let mut file = fs::File::create(out).context("Failed to create output executable")?;

    // Create a Windows batch script with improved queue system for concurrent execution
    let script = format!(
        r#"@echo off
setlocal enabledelayedexpansion

REM Determine cache directory
set "CACHE_DIR=%LOCALAPPDATA%\banderole"
set "APP_DIR=!CACHE_DIR!\{}"
set "LOCK_FILE=!APP_DIR!\.lock"
set "READY_FILE=!APP_DIR!\.ready"
set "QUEUE_DIR=!APP_DIR!\.queue"
set "EXTRACTION_PID_FILE=!APP_DIR!\.extraction.pid"

REM Function to run the application
:run_app
cd /d "!APP_DIR!\app"

REM Find main script
for /f "delims=" %%i in ('"!APP_DIR!\node\node.exe" -e "try {{ console.log(require('./package.json').main || 'index.js'); }} catch(e) {{ console.log('index.js'); }}" 2^>nul') do set "MAIN_SCRIPT=%%i"
if "!MAIN_SCRIPT!"=="" set "MAIN_SCRIPT=index.js"

if exist "!MAIN_SCRIPT!" (
    "!APP_DIR!\node\node.exe" "!MAIN_SCRIPT!" %*
    exit /b !errorlevel!
) else (
    echo Error: Main script '!MAIN_SCRIPT!' not found >&2
    exit /b 1
)

REM Function to clean up queue entry
:cleanup_queue
if defined QUEUE_ENTRY if exist "!QUEUE_ENTRY!" (
    del "!QUEUE_ENTRY!" 2>nul
)
exit /b

REM Check if already extracted and ready
if exist "!READY_FILE!" if exist "!APP_DIR!\app\package.json" if exist "!APP_DIR!\node\node.exe" (
    goto run_app
)

REM Create queue directory if it doesn't exist
if not exist "!QUEUE_DIR!" mkdir "!QUEUE_DIR!" 2>nul

REM Generate unique queue entry
set "QUEUE_ENTRY=!QUEUE_DIR!\%RANDOM%-%TIME:~6,5%.queue"
echo %RANDOM% > "!QUEUE_ENTRY!"

REM Try to acquire lock for extraction
:acquire_lock
if not exist "!LOCK_FILE!" (
    mkdir "!LOCK_FILE!" 2>nul
    if !errorlevel! equ 0 (
        REM We got the lock, record our PID for extraction
        echo !RANDOM! > "!EXTRACTION_PID_FILE!"
        goto extract
    )
)

REM Another process is extracting, wait in queue
:wait_for_ready
if exist "!READY_FILE!" (
    REM Wait for our turn in the queue
    :wait_queue_turn
    if not exist "!QUEUE_ENTRY!" goto run_app
    if exist "!READY_FILE!" goto run_app
    
    REM Check if we're first in queue (simplified check)
    timeout /t 1 /nobreak >nul 2>&1
    goto wait_queue_turn
)

REM Check if extraction process is still alive (simplified for Windows)
if exist "!EXTRACTION_PID_FILE!" (
    timeout /t 1 /nobreak >nul 2>&1
    goto wait_for_ready
) else (
    REM Extraction process may have died, try to acquire lock again
    goto acquire_lock
)

:extract
REM Check again if extraction completed while we were waiting for lock
if exist "!READY_FILE!" if exist "!APP_DIR!\app\package.json" if exist "!APP_DIR!\node\node.exe" (
    call :cleanup_queue
    rmdir "!LOCK_FILE!" 2>nul
    goto run_app
)

REM Extract application
if not exist "!CACHE_DIR!" mkdir "!CACHE_DIR!"
if not exist "!APP_DIR!" mkdir "!APP_DIR!"

REM Create temp file for zip
set "TEMP_ZIP=%TEMP%\banderole-bundle-%RANDOM%.zip"

REM Extract embedded zip data using PowerShell
powershell -NoProfile -Command "$content = Get-Content '%~f0' -Raw; $dataStart = $content.IndexOf('__DATA__') + 8; $data = $content.Substring($dataStart).Trim(); [System.IO.File]::WriteAllBytes('%TEMP_ZIP%', [System.Convert]::FromBase64String($data))"

if not exist "%TEMP_ZIP%" (
    echo Error: Failed to extract bundle data >&2
    call :cleanup_queue
    rmdir "!LOCK_FILE!" 2>nul
    del "!EXTRACTION_PID_FILE!" 2>nul
    exit /b 1
)

REM Extract the bundle using PowerShell
powershell -NoProfile -Command "try {{ Expand-Archive -Path '%TEMP_ZIP%' -DestinationPath '!APP_DIR!' -Force }} catch {{ Write-Error $_.Exception.Message; exit 1 }}"
set "EXTRACT_RESULT=!errorlevel!"
del "%TEMP_ZIP%" 2>nul

if !EXTRACT_RESULT! neq 0 (
    echo Error: Failed to extract bundle >&2
    call :cleanup_queue
    rmdir /s /q "!APP_DIR!" 2>nul
    rmdir "!LOCK_FILE!" 2>nul
    del "!EXTRACTION_PID_FILE!" 2>nul
    exit /b 1
)

REM Verify extraction worked
if not exist "!APP_DIR!\app\package.json" (
    echo Error: Bundle extraction incomplete >&2
    call :cleanup_queue
    rmdir /s /q "!APP_DIR!" 2>nul
    rmdir "!LOCK_FILE!" 2>nul
    del "!EXTRACTION_PID_FILE!" 2>nul
    exit /b 1
)

if not exist "!APP_DIR!\node\node.exe" (
    echo Error: Node.js executable not found >&2
    call :cleanup_queue
    rmdir /s /q "!APP_DIR!" 2>nul
    rmdir "!LOCK_FILE!" 2>nul
    del "!EXTRACTION_PID_FILE!" 2>nul
    exit /b 1
)

REM Mark as ready for other processes
echo ready > "!READY_FILE!"

REM Process queue in order - clean up queue files to wake up waiting processes
if exist "!QUEUE_DIR!" (
    for %%f in ("!QUEUE_DIR!\*.queue") do (
        if exist "%%f" del "%%f" 2>nul
    )
)

REM Clean up extraction metadata
del "!EXTRACTION_PID_FILE!" 2>nul
call :cleanup_queue

REM Clean up lock
rmdir "!LOCK_FILE!" 2>nul

REM Run the application
goto run_app

__DATA__
"#,
        build_id
    );

    file.write_all(script.as_bytes())?;

    // Append base64-encoded zip data
    let encoded = base64::engine::general_purpose::STANDARD.encode(&zip_data);
    file.write_all(encoded.as_bytes())?;

    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// Utility helpers
// ────────────────────────────────────────────────────────────────────────────

fn add_dir_to_zip<W>(
    zip: &mut ZipWriter<W>,
    src_dir: &Path,
    dest_dir: &Path,
    opts: zip::write::FileOptions<'static, ()>,
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

        // Process regular files and symlinks, skip other special files
        if !entry.file_type().is_file() && !entry.file_type().is_symlink() {
            continue;
        }

        // Get file permissions to preserve executable bits (Unix only)
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
    }
    Ok(())
}

/// Add directory to zip without following symlinks but preserving them
fn add_dir_to_zip_no_follow<W>(
    zip: &mut ZipWriter<W>,
    src_dir: &Path,
    dest_dir: &Path,
    opts: zip::write::FileOptions<'static, ()>,
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

        // Process regular files and symlinks, skip other special files
        if !entry.file_type().is_file() && !entry.file_type().is_symlink() {
            continue;
        }

        // Get file permissions to preserve executable bits (Unix only)
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
            // For symlinks, read the target and store it as file content
            // This won't create actual symlinks but avoids infinite loops
            if let Ok(target) = fs::read_link(path) {
                let target_str = target.to_string_lossy();
                zip.write_all(target_str.as_bytes())?;
            }
        } else {
            // For regular files, read the content
            let data = fs::read(path).context("Failed to read file while zipping")?;
            zip.write_all(&data)?;
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
            // Only create subdirectories within the package, not the main app/node_modules path
            if !rel_path.as_os_str().is_empty() {
                zip.add_directory(zip_path.to_string_lossy().as_ref(), opts)?;
            }
            continue;
        }

        // Process regular files and symlinks, skip other special files
        if !entry.file_type().is_file() && !entry.file_type().is_symlink() {
            continue;
        }

        // Get file permissions to preserve executable bits (Unix only)
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
            // For symlinks, read the target and store it as file content
            // This won't create actual symlinks but avoids infinite loops
            if let Ok(target) = fs::read_link(path) {
                let target_str = target.to_string_lossy();
                zip.write_all(target_str.as_bytes())?;
            }
        } else {
            // For regular files, read the content
            let data = fs::read(path).context("Failed to read file while zipping")?;
            zip.write_all(&data)?;
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
) -> Result<()>
where
    W: Write + Read + std::io::Seek,
{
    for entry in walkdir::WalkDir::new(src_dir).follow_links(true) {
        let entry = entry?;
        let path = entry.path();
        let rel_path = path.strip_prefix(src_dir).unwrap();
        let zip_path = dest_dir.join(rel_path);

        // Exclude node_modules from the source directory
        if rel_path.starts_with("node_modules") {
            continue;
        }

        if entry.file_type().is_dir() {
            zip.add_directory(zip_path.to_string_lossy().as_ref(), opts)?;
            continue;
        }

        // Process regular files and symlinks, skip other special files
        if !entry.file_type().is_file() && !entry.file_type().is_symlink() {
            continue;
        }

        // Get file permissions to preserve executable bits (Unix only)
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
    }
    Ok(())
}

/// Copy a package from workspace node_modules (for regular npm/yarn workspaces)
fn copy_workspace_package<W>(
    zip: &mut ZipWriter<W>,
    node_modules_path: &Path,
    package_name: &str,
    opts: zip::write::FileOptions<'static, ()>,
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
            add_dir_to_zip_no_follow_skip_parents(zip, &target_path, &dest_path, opts)?;
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

    // If already resolved, skip
    if resolved.contains(package_name) {
        return Ok(());
    }

    resolved.insert(package_name.to_string());

    // Try to find the package and read its package.json
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
        // Add production dependencies
        if let Some(deps) = package_json["dependencies"].as_object() {
            for dep_name in deps.keys() {
                resolve_workspace_dependencies(node_modules_path, dep_name, resolved, depth + 1)?;
            }
        }

        // Also include peerDependencies that are actually installed
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

        // Also include optionalDependencies that are actually installed
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
