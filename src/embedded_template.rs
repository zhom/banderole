use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Embedded template files
pub struct EmbeddedTemplate {
    pub cargo_toml: &'static str,
    pub build_rs: &'static str,
    pub main_rs: &'static str,
}

impl EmbeddedTemplate {
    /// Get the embedded template files
    pub fn new() -> Self {
        Self {
            cargo_toml: include_str!("../template/Cargo.toml"),
            build_rs: include_str!("../template/build.rs"),
            main_rs: include_str!("../template/src/main.rs"),
        }
    }

    /// Write the template files to a build directory
    pub fn write_to_dir(&self, build_dir: &Path) -> Result<()> {
        // Create the src directory
        let src_dir = build_dir.join("src");
        fs::create_dir_all(&src_dir).context("Failed to create src directory")?;

        // Write Cargo.toml
        let cargo_toml_path = build_dir.join("Cargo.toml");
        fs::write(&cargo_toml_path, self.cargo_toml).context("Failed to write Cargo.toml")?;

        // Write build.rs
        let build_rs_path = build_dir.join("build.rs");
        fs::write(&build_rs_path, self.build_rs).context("Failed to write build.rs")?;

        // Write src/main.rs
        let main_rs_path = src_dir.join("main.rs");
        fs::write(&main_rs_path, self.main_rs).context("Failed to write src/main.rs")?;

        Ok(())
    }
}
