use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use zip::ZipArchive;

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
    
    // Extract application if needed
    extract_application(&app_dir)?;
    
    // Mark as ready
    fs::write(&ready_file, "ready")?;
    
    // Run the application
    run_app(&app_dir, &args[1..])
}

fn get_cache_dir() -> Result<PathBuf> {
    let cache_dir = if let Some(xdg_cache) = env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(xdg_cache).join("banderole")
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".cache").join("banderole")
    } else if cfg!(windows) {
        if let Some(appdata) = env::var_os("LOCALAPPDATA") {
            PathBuf::from(appdata).join("banderole")
        } else if let Some(temp) = env::var_os("TEMP") {
            PathBuf::from(temp).join("banderole-cache")
        } else {
            PathBuf::from("C:\\temp\\banderole-cache")
        }
    } else {
        PathBuf::from("/tmp/banderole-cache")
    };
    
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
    // Create app directory
    fs::create_dir_all(app_dir).context("Failed to create app directory")?;
    
    // Extract embedded zip data
    let cursor = Cursor::new(ZIP_DATA);
    let mut archive = ZipArchive::new(cursor).context("Failed to open embedded zip archive")?;
    
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Failed to read zip entry")?;
        let outpath = app_dir.join(file.name());
        
        if file.name().ends_with('/') {
            // Directory
            fs::create_dir_all(&outpath).context("Failed to create directory")?;
        } else {
            // File
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent).context("Failed to create parent directory")?;
            }
            
            let mut outfile = fs::File::create(&outpath).context("Failed to create output file")?;
            std::io::copy(&mut file, &mut outfile).context("Failed to extract file")?;
            
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
