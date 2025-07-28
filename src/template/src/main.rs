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
    let cache_dir = get_cache_dir()?;
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
        .context("Failed to create lock file")?;
    
    // Acquire exclusive lock
    lock_file.lock_exclusive().context("Failed to acquire extraction lock")?;
    
    // Double-check if extraction completed while waiting for lock
    if ready_file.exists() && is_extraction_valid(&app_dir)? {
        // Release lock and run
        lock_file.unlock().ok();
        return run_app(&app_dir, &args[1..]);
    }
    
    // Extract application if needed
    extract_application(&app_dir)?;
    
    // Mark as ready
    fs::write(&ready_file, "ready")?;
    
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

fn is_extraction_valid(app_dir: &Path) -> Result<bool> {
    let app_package_json = app_dir.join("app").join("package.json");
    let node_executable = if cfg!(windows) {
        app_dir.join("node").join("node.exe")
    } else {
        app_dir.join("node").join("bin").join("node")
    };
    
    Ok(app_package_json.exists() && node_executable.exists())
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
        
        // Normalize path separators for the current platform
        // Zip files always use forward slashes, convert to platform-specific separators
        let normalized_name = if cfg!(windows) {
            file_name.replace('/', "\\")
        } else {
            file_name.to_string()
        };
        
        // Create the output path using platform-specific path handling
        let outpath = app_dir.join(&normalized_name);
        
        // Ensure the path is within the app directory (security check)
        if !outpath.starts_with(app_dir) {
            continue;
        }
        
        if file_name.ends_with('/') {
            // Directory
            fs::create_dir_all(&outpath).context("Failed to create directory")?;
        } else {
            // File
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent).context("Failed to create parent directory")?;
            }
            
            let mut outfile = fs::File::create(&outpath).context("Failed to create output file")?;
            std::io::copy(&mut file, &mut outfile).context("Failed to extract file")?;
            
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
    let node_executable = if cfg!(windows) {
        app_dir.join("node").join("node.exe")
    } else {
        app_dir.join("node").join("bin").join("node")
    };
    
    // Change to app directory
    env::set_current_dir(&app_path).context("Failed to change to app directory")?;
    
    // Find main script from package.json
    let main_script = find_main_script(&app_path)?;
    
    // Build command arguments
    let mut cmd_args = vec![main_script];
    cmd_args.extend(args.iter().cloned());
    
    // Execute Node.js application
    let status = Command::new(&node_executable)
        .args(&cmd_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to execute Node.js application")?;
    
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
