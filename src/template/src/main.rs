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
        
        // Determine if this is a directory entry
        let is_directory = file_name.ends_with('/');
        
        // Remove trailing slash for proper path construction
        let clean_file_name = if is_directory {
            file_name.trim_end_matches('/')
        } else {
            file_name
        };
        
        // Use proper path handling instead of string replacement
        // Split the path by forward slashes and join using PathBuf for proper platform handling
        let path_components: Vec<&str> = clean_file_name.split('/').collect();
        let mut outpath = app_dir.to_path_buf();
        for component in path_components {
            if !component.is_empty() {
                outpath = outpath.join(component);
            }
        }
        
        // Ensure the path is within the app directory (security check)
        if !outpath.starts_with(app_dir) {
            continue;
        }
        
        if is_directory {
            // Directory entry - create the directory
            fs::create_dir_all(&outpath)
                .with_context(|| format!("Failed to create directory at {}", outpath.display()))?;
        } else {
            // File entry - create parent directories first, then the file
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create parent directory at {}", parent.display()))?;
            }
            
            let mut outfile = fs::File::create(&outpath)
                .with_context(|| format!("Failed to create output file at {}", outpath.display()))?;
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
