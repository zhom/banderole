use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use zip::ZipArchive;
use directories::BaseDirs;
use fs2::FileExt;

// These will be replaced during the build process with actual embedded data
// The build script will generate a data.rs file with the actual data
include!(concat!(env!("OUT_DIR"), "/data.rs"));

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    
    // Get cache directory
    let cache_dir = get_cache_dir().context("Failed to determine cache directory")?;
    let app_dir = cache_dir.join(&BUILD_ID);
    let ready_file = app_dir.join(".ready");
    
    // Check if already extracted and ready
    if ready_file.exists() && is_extraction_valid(&app_dir)? {
        return run_app(&app_dir, &args[1..]);
    }
    
    // Use file locking to prevent concurrent extraction
    let lock_file_path = cache_dir.join(format!("{}.lock", BUILD_ID));
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_file_path)
        .with_context(|| format!("Failed to create lock file at {}", lock_file_path.display()))?;
    
    // Acquire exclusive lock
    lock_file.lock_exclusive().context("Failed to acquire extraction lock")?;
    
    // Double-check if extraction completed while waiting for lock
    if ready_file.exists() && is_extraction_valid(&app_dir)? {
        // Release lock and run
        lock_file.unlock().ok();
        return run_app(&app_dir, &args[1..]);
    }
    
    // Extract application if needed
    extract_application(&app_dir)
        .with_context(|| format!("Failed to extract application to {}", app_dir.display()))?;
    
    // Mark as ready
    fs::write(&ready_file, "ready")
        .with_context(|| format!("Failed to create ready file at {}", ready_file.display()))?;
    
    // Release lock
    lock_file.unlock().context("Failed to release extraction lock")?;
    
    // Run the application
    run_app(&app_dir, &args[1..])
}

fn get_cache_dir() -> Result<PathBuf> {
    let cache_dir = BaseDirs::new().unwrap().cache_dir().join("banderole");    
    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;
    Ok(cache_dir)
}

fn get_node_executable_path(app_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        // On Windows, Node.js is extracted to node/{platform}/node.exe
        // Try to find the platform-specific subdirectory
        let node_dir = app_dir.join("node");
        
        // Look for platform-specific subdirectories
        if let Ok(entries) = fs::read_dir(&node_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let node_exe = path.join("node.exe");
                    if node_exe.exists() {
                        return node_exe;
                    }
                }
            }
        }
        
        // Fallback to direct path (shouldn't happen with new extraction logic)
        node_dir.join("node.exe")
    } else {
        // On Unix systems, Node.js is in node/bin/node
        app_dir.join("node").join("bin").join("node")
    }
}

fn is_extraction_valid(app_dir: &Path) -> Result<bool> {
    let app_package_json = app_dir.join("app").join("package.json");
    let node_executable = get_node_executable_path(app_dir);
    #[cfg(windows)]
    let node_executable = node_executable
        .canonicalize()
        .unwrap_or_else(|_| node_executable.clone());
    
    let package_exists = app_package_json.exists();
    let node_exists = node_executable.exists();
    
    if !package_exists || !node_exists {
        // Log debugging information for failed validation
        eprintln!("Extraction validation failed:");
        eprintln!("  App directory: {}", app_dir.display());
        eprintln!("  Package.json exists: {} ({})", package_exists, app_package_json.display());
        eprintln!("  Node executable exists: {} ({})", node_exists, node_executable.display());
        
        if let Ok(entries) = fs::read_dir(app_dir) {
            eprintln!("  App directory contents:");
            for entry in entries.flatten() {
                eprintln!("    - {}", entry.file_name().to_string_lossy());
            }
        }
        
        if let Ok(entries) = fs::read_dir(app_dir.join("node")) {
            eprintln!("  Node directory contents:");
            for entry in entries.flatten() {
                eprintln!("    - {}", entry.file_name().to_string_lossy());
            }
        }
    }
    
    Ok(package_exists && node_exists)
}

fn extract_application(app_dir: &Path) -> Result<()> {
    // Remove existing directory if it exists to ensure clean extraction
    if app_dir.exists() {
        fs::remove_dir_all(app_dir).context("Failed to remove existing app directory")?;
    }
    
    // Create app directory
    fs::create_dir_all(app_dir).context("Failed to create app directory")?;
    
    // Extract embedded zip data
    let cursor = Cursor::new(ZIP_DATA);
    let mut archive = ZipArchive::new(cursor).context("Failed to open embedded zip archive")?;
    
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Failed to read zip entry")?;
        
        // Get the file name from the zip entry
        let file_name = file.name();
        
        // Skip entries with invalid characters or paths
        if file_name.is_empty() || file_name.contains('\0') {
            continue;
        }
        
        // Determine if this is a directory entry
        let is_directory = file_name.ends_with('/') || file.is_dir();
        
        // Skip empty directory entries that are just the trailing slash
        if is_directory && (file_name == "/" || file_name.trim_matches('/').is_empty()) {
            continue;
        }
        
        // Remove trailing slash for proper path construction
        let clean_file_name = if is_directory {
            file_name.trim_end_matches('/')
        } else {
            file_name
        };
        
        // Skip if the cleaned name is empty (shouldn't happen but be safe)
        if clean_file_name.is_empty() {
            continue;
        }
        
        // Use proper path handling instead of string replacement
        // Split the path by forward slashes and join using PathBuf for proper platform handling
        let path_components: Vec<&str> = clean_file_name.split('/').filter(|s| !s.is_empty()).collect();
        
        // Skip if no valid path components
        if path_components.is_empty() {
            continue;
        }
        
        let mut outpath = app_dir.to_path_buf();
        for component in path_components {
            outpath = outpath.join(component);
        }
        
        // Ensure the path is within the app directory (security check)
        if !outpath.starts_with(app_dir) {
            continue;
        }
        
        if is_directory {
            // Directory entry - create the directory
            fs::create_dir_all(&outpath)
                .with_context(|| format!("Failed to create directory '{}' from zip entry '{}'", outpath.display(), file_name))?;
        } else {
            // File entry - create parent directories first, then the file
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create parent directory '{}' for file '{}'", parent.display(), outpath.display()))?;
            }
            
            let mut outfile = fs::File::create(&outpath)
                .with_context(|| format!("Failed to create output file '{}' from zip entry '{}'", outpath.display(), file_name))?;
            std::io::copy(&mut file, &mut outfile)
                .with_context(|| format!("Failed to extract file to {}", outpath.display()))?;
            
            // Ensure file is fully written before setting permissions
            outfile.sync_all().context("Failed to sync file to disk")?;
            drop(outfile); // Explicitly close the file
            
            // Set executable permissions on Unix systems
            #[cfg(unix)]
            {
                if let Some(mode) = file.unix_mode() {
                    use std::os::unix::fs::PermissionsExt;
                    let permissions = std::fs::Permissions::from_mode(mode);
                    fs::set_permissions(&outpath, permissions).context("Failed to set permissions")?;
                }
            }
        }
    }
    
    Ok(())
}

fn run_app(app_dir: &Path, args: &[String]) -> Result<()> {
    let app_path = app_dir.join("app");
    let node_executable = get_node_executable_path(app_dir);
    
    // Verify Node.js executable exists and is accessible
    if !node_executable.exists() {
        let app_dir_contents = fs::read_dir(&app_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|e| vec![format!("Error reading app dir: {}", e)]);
            
        let node_dir_contents = fs::read_dir(app_dir.join("node"))
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|e| vec![format!("Error reading node dir: {}", e)]);
            
        return Err(anyhow::anyhow!(
            "Node.js executable not found at: {}\nPlatform: {} {}\nApp directory contents: {:?}\nNode directory contents: {:?}",
            node_executable.display(),
            std::env::consts::OS,
            std::env::consts::ARCH,
            app_dir_contents,
            node_dir_contents
        ));
    }
    
    // On Windows, verify the executable is actually executable
    #[cfg(windows)]
    {
        if let Ok(metadata) = fs::metadata(&node_executable) {
            if !metadata.is_file() {
                return Err(anyhow::anyhow!(
                    "Node.js executable path exists but is not a file: {}", 
                    node_executable.display()
                ));
            }
        } else {
            return Err(anyhow::anyhow!(
                "Cannot read metadata for Node.js executable: {}", 
                node_executable.display()
            ));
        }
    }
    
    // Verify app directory exists
    if !app_path.exists() {
        return Err(anyhow::anyhow!(
            "App directory not found at: {}", 
            app_path.display()
        ));
    }
    
    // Change to app directory
    env::set_current_dir(&app_path)
        .with_context(|| format!("Failed to change to app directory: {}", app_path.display()))?;
    
    // Find main script from package.json
    let main_script = find_main_script(&app_path)?;
    
    // Build command arguments
    let mut cmd_args = vec![main_script.clone()];
    cmd_args.extend(args.iter().cloned());
    
    // Execute Node.js application with a few retries to tolerate transient Windows issues
    let mut status = None;
    let mut last_err: Option<anyhow::Error> = None;
    let max_attempts: u32 = 8;
    for attempt in 1..=max_attempts {
        match Command::new(&node_executable)
            .args(&cmd_args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
        {
            Ok(s) => {
                status = Some(s);
                break;
            }
            Err(e) => {
                last_err = Some(anyhow::anyhow!(e).context(format!(
                    "Failed to execute Node.js application (attempt {attempt}/{max_attempts})\nExecutable: {}\nMain script: {}\nArgs: {:?}\nWorking directory: {}",
                    node_executable.display(),
                    main_script,
                    cmd_args,
                    app_path.display()
                )));
                #[cfg(windows)]
                {
                    use std::time::Duration;
                    std::thread::sleep(Duration::from_millis(50 * attempt as u64));
                }
                #[cfg(not(windows))]
                {
                    if attempt >= 2 {
                        break;
                    }
                }
            }
        }
    }
    let status = status.ok_or_else(|| last_err.unwrap_or_else(|| anyhow::anyhow!(
        "Failed to execute Node.js application after {} attempts",
        max_attempts
    )))?;
    
    std::process::exit(status.code().unwrap_or(1));
}

fn find_main_script(app_path: &Path) -> Result<String> {
    let package_json_path = app_path.join("package.json");
    
    if package_json_path.exists() {
        let package_content = fs::read_to_string(&package_json_path)
            .context("Failed to read package.json")?;
        
        if let Ok(package_json) = serde_json::from_str::<serde_json::Value>(&package_content) {
            if let Some(main) = package_json["main"].as_str() {
                return Ok(main.to_string());
            }
        }
    }
    
    // Default to index.js
    Ok("index.js".to_string())
}
