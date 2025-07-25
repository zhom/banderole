use crate::node_downloader::NodeDownloader;
use crate::platform::Platform;
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;
use base64::Engine as _;
use zip::ZipWriter;

/// Public entry-point used by `main.rs`.
///
/// * `project_path` – path that contains a `package.json`.
/// * `output_path`  – optional path to the produced bundle file. If omitted, an
///   automatically-generated name is used.
/// * `custom_name` – optional custom name for the executable.
///
/// The implementation uses a simpler, more reliable approach based on Playwright's bundling strategy.
pub async fn bundle_project(project_path: PathBuf, output_path: Option<PathBuf>, custom_name: Option<String>) -> Result<()> {
    // 1. Validate & canonicalize input directory.
    let project_path = project_path
        .canonicalize()
        .context("Failed to resolve project path")?;
    let pkg_json = project_path.join("package.json");
    anyhow::ensure!(pkg_json.exists(), "package.json not found in {}", project_path.display());

    // 2. Read `package.json` for name/version and detect project structure
    let package_content = fs::read_to_string(&pkg_json).context("Failed to read package.json")?;
    let package_value: Value = serde_json::from_str(&package_content).context("Failed to parse package.json")?;
    
    let (app_name, app_version) = (
        package_value["name"].as_str().unwrap_or("app").to_string(),
        package_value["version"].as_str().unwrap_or("0.0.0").to_string(),
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
        let opts: zip::write::FileOptions<'static, ()> = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        // Copy the determined source directory
        add_dir_to_zip(&mut zip, &source_dir, Path::new("app"), opts)?;
        
        // If we're using a subdirectory, also copy the root package.json with adjusted paths
        if source_dir != project_path {
            let root_package_json = project_path.join("package.json");
            if root_package_json.exists() {
                zip.start_file("app/package.json", opts)?;
                
                // Read and modify package.json to adjust the main path
                let content = fs::read_to_string(&root_package_json).context("Failed to read root package.json")?;
                let mut package_value: Value = serde_json::from_str(&content).context("Failed to parse root package.json")?;
                
                // Adjust the main field if it points to the source directory
                if let Some(main) = package_value["main"].as_str() {
                    let main_path = project_path.join(main);
                    if let Ok(relative_to_source) = main_path.strip_prefix(&source_dir) {
                        package_value["main"] = Value::String(relative_to_source.to_string_lossy().to_string());
                    }
                }
                
                let modified_content = serde_json::to_string_pretty(&package_value)
                    .context("Failed to serialize modified package.json")?;
                zip.write_all(modified_content.as_bytes())?;
            }
        }
        // Copy Node runtime directory.
        add_dir_to_zip(&mut zip, node_root, Path::new("node"), opts)?;
        zip.finish()?;
    }

    // 8. Build self-extracting launcher using a more reliable approach.
    create_self_extracting_executable(&output_path, zip_data, &app_name)?;

    println!("Bundle created at {}", output_path.display());
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
            let parent_name = parent.file_name()
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
    let content = fs::read_to_string(tsconfig_path)
        .context("Failed to read tsconfig.json")?;
    
    // Remove comments for JSON parsing (simple approach)
    let cleaned_content = content
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    
    let mut config: Value = serde_json::from_str(&cleaned_content)
        .context("Failed to parse tsconfig.json")?;

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
                if let (Some(base_obj), Some(current_obj)) = (base_config.as_object(), config.as_object()) {
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

    let ext = if Platform::current().is_windows() { ".exe" } else { "" };
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

    // Write a simpler, more reliable shell script
    let script = format!(r#"#!/bin/bash
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

# Check if already extracted and ready
if [ -f "$APP_DIR/app/package.json" ] && [ -x "$APP_DIR/node/bin/node" ]; then
    # Already extracted, run directly
    cd "$APP_DIR/app"
    
    # Find main script from package.json
    MAIN_SCRIPT=$("$APP_DIR/node/bin/node" -e "try {{ console.log(require('./package.json').main || 'index.js'); }} catch(e) {{ console.log('index.js'); }}" 2>/dev/null || echo "index.js")
    
    if [ -f "$MAIN_SCRIPT" ]; then
        exec "$APP_DIR/node/bin/node" "$MAIN_SCRIPT" "$@"
    else
        echo "Error: Main script '$MAIN_SCRIPT' not found" >&2
        exit 1
    fi
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

# Run the application
cd "$APP_DIR/app"
MAIN_SCRIPT=$("$APP_DIR/node/bin/node" -e "try {{ console.log(require('./package.json').main || 'index.js'); }} catch(e) {{ console.log('index.js'); }}" 2>/dev/null || echo "index.js")

if [ -f "$MAIN_SCRIPT" ]; then
    exec "$APP_DIR/node/bin/node" "$MAIN_SCRIPT" "$@"
else
    echo "Error: Main script '$MAIN_SCRIPT' not found" >&2
    exit 1
fi

__DATA__
"#, build_id);

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

    // Create a more reliable Windows batch script
    let script = format!(r#"@echo off
setlocal enabledelayedexpansion

REM Determine cache directory
set "CACHE_DIR=%LOCALAPPDATA%\banderole"
set "APP_DIR=!CACHE_DIR!\{}"

REM Check if already extracted and ready
if exist "!APP_DIR!\app\package.json" if exist "!APP_DIR!\node\node.exe" (
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
    exit /b 1
)

REM Extract the bundle using PowerShell
powershell -NoProfile -Command "try {{ Expand-Archive -Path '%TEMP_ZIP%' -DestinationPath '!APP_DIR!' -Force }} catch {{ Write-Error $_.Exception.Message; exit 1 }}"
set "EXTRACT_RESULT=!errorlevel!"
del "%TEMP_ZIP%" 2>nul

if !EXTRACT_RESULT! neq 0 (
    echo Error: Failed to extract bundle >&2
    rmdir /s /q "!APP_DIR!" 2>nul
    exit /b 1
)

REM Verify extraction worked
if not exist "!APP_DIR!\app\package.json" (
    echo Error: Bundle extraction incomplete >&2
    rmdir /s /q "!APP_DIR!" 2>nul
    exit /b 1
)

if not exist "!APP_DIR!\node\node.exe" (
    echo Error: Node.js executable not found >&2
    rmdir /s /q "!APP_DIR!" 2>nul
    exit /b 1
)

REM Run the application
cd /d "!APP_DIR!\app"
for /f "delims=" %%i in ('"!APP_DIR!\node\node.exe" -e "try {{ console.log(require('./package.json').main || 'index.js'); }} catch(e) {{ console.log('index.js'); }}" 2^>nul') do set "MAIN_SCRIPT=%%i"
if "!MAIN_SCRIPT!"=="" set "MAIN_SCRIPT=index.js"

if exist "!MAIN_SCRIPT!" (
    "!APP_DIR!\node\node.exe" "!MAIN_SCRIPT!" %*
    exit /b !errorlevel!
) else (
    echo Error: Main script '!MAIN_SCRIPT!' not found >&2
    exit /b 1
)

__DATA__
"#, build_id);

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
    use std::os::unix::fs::PermissionsExt;

    for entry in walkdir::WalkDir::new(src_dir) {
        let entry = entry?;
        let path = entry.path();
        let rel_path = path.strip_prefix(src_dir).unwrap();
        let zip_path = dest_dir.join(rel_path);

        if entry.file_type().is_dir() {
            zip.add_directory(zip_path.to_string_lossy().as_ref(), opts)?;
            continue;
        }

        // Get file permissions to preserve executable bits
        let metadata = fs::metadata(path)?;
        let permissions = metadata.permissions();
        let mode = permissions.mode();
        
        // Create file options with Unix permissions
        let file_opts = opts.unix_permissions(mode);
        
        zip.start_file(zip_path.to_string_lossy().as_ref(), file_opts)?;
        let data = fs::read(path).context("Failed to read file while zipping")?;
        zip.write_all(&data)?;
    }
    Ok(())
}
