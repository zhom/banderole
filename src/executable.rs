use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use uuid::Uuid;

use crate::embedded_template::EmbeddedTemplate;
use crate::platform::Platform;
use crate::rust_toolchain::RustToolchain;

/// Create a cross-platform Rust executable with embedded data
pub fn create_self_extracting_executable(
    output_path: &Path,
    zip_data: Vec<u8>,
    app_name: &str,
) -> Result<()> {
    // Check if Rust toolchain is available
    if let Err(e) = RustToolchain::check_availability() {
        eprintln!("\nError: {}", e);
        eprintln!("{}", RustToolchain::get_installation_instructions());
        return Err(e);
    }
    
    let build_id = Uuid::new_v4().to_string();
    
    // Create temporary directory for building
    let temp_dir = TempDir::new().context("Failed to create temporary directory")?;
    let build_dir = temp_dir.path();
    
    // Copy template to build directory
    copy_template_to_build_dir(build_dir)?;
    
    // Write embedded data
    let zip_path = build_dir.join("embedded_data.zip");
    fs::write(&zip_path, &zip_data).context("Failed to write embedded zip data")?;
    
    let build_id_path = build_dir.join("build_id.txt");
    fs::write(&build_id_path, &build_id).context("Failed to write build ID")?;
    
    // Update Cargo.toml with app name
    update_cargo_toml(build_dir, app_name)?;
    
    // Build the executable
    build_executable(build_dir, output_path, app_name)?;
    
    Ok(())
}

fn copy_template_to_build_dir(build_dir: &Path) -> Result<()> {
    // Use embedded template files instead of filesystem copy
    let template = EmbeddedTemplate::new();
    template.write_to_dir(build_dir)
        .context("Failed to write embedded template files to build directory")?;
    
    Ok(())
}



fn update_cargo_toml(build_dir: &Path, app_name: &str) -> Result<()> {
    let cargo_toml_path = build_dir.join("Cargo.toml");
    let cargo_content = fs::read_to_string(&cargo_toml_path)
        .context("Failed to read Cargo.toml")?;
    
    // Replace the package name
    let updated_content = cargo_content.replace(
        r#"name = "banderole-app""#,
        &format!(r#"name = "{}""#, sanitize_package_name(app_name))
    );
    
    fs::write(&cargo_toml_path, updated_content)
        .context("Failed to write updated Cargo.toml")?;
    
    Ok(())
}

fn sanitize_package_name(name: &str) -> String {
    // Rust package names must be valid identifiers
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect::<String>()
        .trim_start_matches(|c: char| c.is_numeric() || c == '-')
        .to_string()
}

fn build_executable(build_dir: &Path, output_path: &Path, app_name: &str) -> Result<()> {
    let current_platform = Platform::current();
    let target_triple = get_target_triple(&current_platform);
    
    // Ensure we have the target installed
    install_rust_target(&target_triple)?;
    
    // Build the executable
    let mut cmd = Command::new("cargo");
    cmd.current_dir(build_dir)
        .args(&["build", "--release", "--target", &target_triple]);
    
    let output = cmd.output().context("Failed to execute cargo build")?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Cargo build failed:\n{}", stderr);
    }
    
    // Get the sanitized package name to find the correct executable
    let package_name = sanitize_package_name(app_name);
    let executable_name = if current_platform.is_windows() {
        format!("{}.exe", package_name)
    } else {
        package_name
    };
    
    let built_executable = build_dir
        .join("target")
        .join(&target_triple)
        .join("release")
        .join(executable_name);
    
    if !built_executable.exists() {
        anyhow::bail!("Built executable not found at {}", built_executable.display());
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

