use anyhow::{Context, Result};
use log::{debug, info};
use std::process::Command;

/// Manages Rust toolchain requirements and installation
pub struct RustToolchain;

impl RustToolchain {
    /// Check if Rust toolchain is available and properly configured
    pub fn check_availability() -> Result<()> {
        // Check if rustc is available
        let rustc_output = Command::new("rustc").arg("--version").output();

        match rustc_output {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout);
                debug!("Found Rust compiler: {}", version.trim());
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Rust compiler (rustc) not found. Please install Rust from https://rustup.rs/"
                ));
            }
        }

        // Check if cargo is available
        let cargo_output = Command::new("cargo").arg("--version").output();

        match cargo_output {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout);
                debug!("Found Cargo: {}", version.trim());
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Cargo not found. Please install Rust from https://rustup.rs/"
                ));
            }
        }

        // Check if rustup is available (for target management)
        let rustup_output = Command::new("rustup").arg("--version").output();

        match rustup_output {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout);
                debug!("Found rustup: {}", version.trim());
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "rustup not found. Please install Rust from https://rustup.rs/"
                ));
            }
        }

        Ok(())
    }

    /// Install a Rust target if not already installed
    pub fn ensure_target_installed(target: &str) -> Result<()> {
        // Check if target is already installed
        let output = Command::new("rustup")
            .args(["target", "list", "--installed"])
            .output()
            .context("Failed to check installed targets")?;

        let installed_targets = String::from_utf8_lossy(&output.stdout);

        if !installed_targets.contains(target) {
            info!("Installing Rust target: {target}");
            let install_output = Command::new("rustup")
                .args(["target", "add", target])
                .output()
                .context("Failed to install Rust target")?;

            if !install_output.status.success() {
                let stderr = String::from_utf8_lossy(&install_output.stderr);
                anyhow::bail!("Failed to install target {}:\n{}", target, stderr);
            }
            info!("Successfully installed target: {target}");
        } else {
            debug!("Target {target} is already installed");
        }

        Ok(())
    }

    /// Get helpful installation instructions for the user
    pub fn get_installation_instructions() -> String {
        r#"
Rust toolchain is required to build portable executables.

To install Rust:
1. Visit https://rustup.rs/
2. Follow the installation instructions for your platform
3. Restart your terminal/command prompt
4. Verify installation with: rustc --version

For automated installation:
- Linux/macOS: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
- Windows: Download and run rustup-init.exe from https://rustup.rs/

After installation, you can use banderole to create portable Node.js executables
without requiring users to have Node.js or Rust installed.
"#
        .to_string()
    }
}
