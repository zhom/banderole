#![allow(dead_code)]

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

/// Represents different project types for testing
#[derive(Debug, Clone)]
pub enum ProjectType {
    Simple,
    TypeScript { out_dir: String },
    Workspace,
    PnpmWorkspace,
}

/// Represents a test project configuration
#[derive(Debug, Clone)]
pub struct TestProject {
    pub name: String,
    pub project_type: ProjectType,
    pub dependencies: Vec<(String, String)>, // (name, version)
    pub dev_dependencies: Vec<(String, String)>,
    pub has_nvmrc: Option<String>,
    pub has_node_version: Option<String>,
}

impl Default for TestProject {
    fn default() -> Self {
        Self {
            name: "test-project".to_string(),
            project_type: ProjectType::Simple,
            dependencies: vec![],
            dev_dependencies: vec![],
            has_nvmrc: None,
            has_node_version: None,
        }
    }
}

impl TestProject {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Default::default()
        }
    }

    pub fn with_dependency(mut self, name: &str, version: &str) -> Self {
        self.dependencies
            .push((name.to_string(), version.to_string()));
        self
    }

    pub fn with_dev_dependency(mut self, name: &str, version: &str) -> Self {
        self.dev_dependencies
            .push((name.to_string(), version.to_string()));
        self
    }

    pub fn with_nvmrc(mut self, version: &str) -> Self {
        self.has_nvmrc = Some(version.to_string());
        self
    }

    pub fn with_node_version(mut self, version: &str) -> Self {
        self.has_node_version = Some(version.to_string());
        self
    }

    pub fn typescript(mut self, out_dir: &str) -> Self {
        self.project_type = ProjectType::TypeScript {
            out_dir: out_dir.to_string(),
        };
        self
    }

    pub fn workspace(mut self) -> Self {
        self.project_type = ProjectType::Workspace;
        self
    }

    pub fn pnpm_workspace(mut self) -> Self {
        self.project_type = ProjectType::PnpmWorkspace;
        self
    }
}

/// Test project manager for creating and managing test projects
pub struct TestProjectManager {
    temp_dir: TempDir,
    project_path: PathBuf,
    workspace_root: Option<PathBuf>,
}

impl TestProjectManager {
    /// Create a new test project in a temporary directory
    pub fn create(config: TestProject) -> Result<Self> {
        let temp_dir = TempDir::new()?;
        let mut manager = Self {
            temp_dir,
            project_path: PathBuf::new(),
            workspace_root: None,
        };

        match config.project_type {
            ProjectType::Simple => {
                manager.project_path = manager.temp_dir.path().join(&config.name);
                manager.create_simple_project(&config)?;
            }
            ProjectType::TypeScript { ref out_dir } => {
                manager.project_path = manager.temp_dir.path().join(&config.name);
                manager.create_typescript_project(&config, out_dir)?;
            }
            ProjectType::Workspace => {
                manager.workspace_root = Some(manager.temp_dir.path().join("workspace"));
                manager.project_path = manager.workspace_root.as_ref().unwrap().join(&config.name);
                manager.create_workspace_project(&config)?;
            }
            ProjectType::PnpmWorkspace => {
                manager.workspace_root = Some(manager.temp_dir.path().join("workspace"));
                manager.project_path = manager.workspace_root.as_ref().unwrap().join(&config.name);
                manager.create_pnpm_workspace_project(&config)?;
            }
        }

        Ok(manager)
    }

    /// Get the path to the project being tested
    pub fn project_path(&self) -> &Path {
        &self.project_path
    }

    /// Get the path to the workspace root (if this is a workspace project)
    pub fn workspace_root(&self) -> Option<&Path> {
        self.workspace_root.as_deref()
    }

    /// Get the temporary directory path
    pub fn temp_dir(&self) -> &Path {
        self.temp_dir.path()
    }

    /// Install dependencies using npm
    pub fn install_dependencies(&self) -> Result<()> {
        let npm_install = Command::new("npm")
            .args(["install"])
            .current_dir(&self.project_path)
            .output()?;

        if !npm_install.status.success() {
            anyhow::bail!(
                "npm install failed: {}",
                String::from_utf8_lossy(&npm_install.stderr)
            );
        }

        Ok(())
    }

    /// Install dependencies using pnpm
    pub fn install_pnpm_dependencies(&self) -> Result<()> {
        // First try pnpm
        let pnpm_install = Command::new("pnpm")
            .args(["install"])
            .current_dir(self.workspace_root.as_ref().unwrap_or(&self.project_path))
            .output();

        match pnpm_install {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => {
                anyhow::bail!(
                    "pnpm install failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(_) => {
                // Fallback to npm if pnpm is not available
                println!("pnpm not found, falling back to npm");
                self.install_dependencies()
            }
        }
    }

    /// Install dependencies in workspace root
    pub fn install_workspace_dependencies(&self) -> Result<()> {
        if let Some(workspace_root) = &self.workspace_root {
            let npm_install = Command::new("npm")
                .args(["install"])
                .current_dir(workspace_root)
                .output()?;

            if !npm_install.status.success() {
                anyhow::bail!(
                    "npm install failed in workspace root: {}",
                    String::from_utf8_lossy(&npm_install.stderr)
                );
            }
        }
        Ok(())
    }

    fn create_simple_project(&self, config: &TestProject) -> Result<()> {
        fs::create_dir_all(&self.project_path)?;

        let package_json = self.generate_package_json(config)?;
        fs::write(self.project_path.join("package.json"), package_json)?;

        let index_js = r#"console.log("Hello from test project!");
console.log("Node version:", process.version);
console.log("Platform:", process.platform);
console.log("Architecture:", process.arch);

// Test environment variables
console.log("Test env var:", process.env.TEST_VAR || 'not set');

// Test process arguments
console.log("Process args:", process.argv.slice(2));

// Test dependencies if any
try {
    const deps = require('./package.json').dependencies || {};
    console.log("Dependencies:", Object.keys(deps));
    
    // Test specific commonly used dependencies
    if (deps['adm-zip']) {
        const AdmZip = require('adm-zip');
        console.log("Successfully loaded adm-zip:", typeof AdmZip);
        
        // Test basic functionality
        const zip = new AdmZip();
        zip.addFile("test.txt", Buffer.from("test content"));
        const entries = zip.getEntries();
        console.log("Zip entries count:", entries.length);
        console.log("DEPENDENCY_TEST_PASSED");
    }
} catch (e) {
    console.error("Dependency test failed:", e.message);
    console.log("DEPENDENCY_TEST_FAILED");
}

console.log("All tests completed!");
process.exit(0);"#;

        fs::write(self.project_path.join("index.js"), index_js)?;

        // Add Node version files if specified
        if let Some(ref version) = config.has_nvmrc {
            fs::write(self.project_path.join(".nvmrc"), version)?;
        }
        if let Some(ref version) = config.has_node_version {
            fs::write(self.project_path.join(".node-version"), version)?;
        }

        Ok(())
    }

    fn create_typescript_project(&self, config: &TestProject, out_dir: &str) -> Result<()> {
        fs::create_dir_all(&self.project_path)?;
        fs::create_dir_all(self.project_path.join(out_dir))?;

        let mut package_json = self.generate_package_json(config)?;
        // Update main to point to compiled output
        let mut package_obj: serde_json::Value = serde_json::from_str(&package_json)?;
        package_obj["main"] = serde_json::Value::String(format!("{out_dir}/index.js"));
        package_json = serde_json::to_string_pretty(&package_obj)?;

        fs::write(self.project_path.join("package.json"), package_json)?;

        let tsconfig_json = format!(
            r#"{{
  "compilerOptions": {{
    "target": "ES2020",
    "module": "commonjs",
    "outDir": "./{out_dir}",
    "rootDir": "./src",
    "strict": true
  }}
}}"#
        );

        fs::write(self.project_path.join("tsconfig.json"), tsconfig_json)?;

        // Create source TypeScript file
        fs::create_dir_all(self.project_path.join("src"))?;
        let src_index_ts = r#"console.log("Hello from TypeScript project!");
console.log("Node version:", process.version);
console.log("This should come from the compiled output directory");
try {
    const marker = require('./marker.js');
    console.log("Marker file found:", marker.source);
} catch (e) {
    console.log("Marker file not found");
}"#;

        fs::write(self.project_path.join("src/index.ts"), src_index_ts)?;

        // Create compiled output
        let compiled_index_js = r#"console.log("Hello from TypeScript project!");
console.log("Node version:", process.version);
console.log("This should come from the compiled output directory");
try {
    const marker = require('./marker.js');
    console.log("Marker file found:", marker.source);
} catch (e) {
    console.log("Marker file not found");
}"#;

        fs::write(
            self.project_path.join(out_dir).join("index.js"),
            compiled_index_js,
        )?;

        // Create a marker file to verify correct source directory is used
        let marker_js = format!(r#"module.exports = {{ source: "{out_dir}" }};"#);
        fs::write(self.project_path.join(out_dir).join("marker.js"), marker_js)?;

        Ok(())
    }

    fn create_workspace_project(&self, config: &TestProject) -> Result<()> {
        let workspace_root = self.workspace_root.as_ref().unwrap();
        fs::create_dir_all(workspace_root)?;
        fs::create_dir_all(&self.project_path)?;

        // Create workspace root package.json
        let workspace_package_json = format!(
            r#"{{
  "name": "test-workspace",
  "version": "1.0.0",
  "private": true,
  "workspaces": [
    "{}"
  ],
  "dependencies": {{
{}
  }}
}}"#,
            config.name.replace("/", "-"), // Replace slashes to make valid package name
            self.format_dependencies(&config.dependencies)
        );

        fs::write(workspace_root.join("package.json"), workspace_package_json)?;

        // Create project package.json
        let project_package_json = self.generate_package_json(config)?;
        fs::write(self.project_path.join("package.json"), project_package_json)?;

        // Create project files
        let index_js = r#"console.log("Hello from workspace project!");
console.log("Node version:", process.version);

// Test workspace dependencies
try {
    const deps = require('./package.json').dependencies || {};
    console.log("Dependencies:", Object.keys(deps));
    
    // Test specific dependencies
    if (deps['adm-zip']) {
        const AdmZip = require('adm-zip');
        console.log("Successfully loaded adm-zip from workspace:", typeof AdmZip);
        
        // Test basic functionality
        const zip = new AdmZip();
        zip.addFile("test.txt", Buffer.from("workspace test content"));
        const entries = zip.getEntries();
        console.log("Zip entries count:", entries.length);
        console.log("WORKSPACE_DEPENDENCY_TEST_PASSED");
    }
} catch (e) {
    console.error("Workspace dependency test failed:", e.message);
    console.log("WORKSPACE_DEPENDENCY_TEST_FAILED");
}

console.log("Workspace project test completed!");
process.exit(0);"#;

        fs::write(self.project_path.join("index.js"), index_js)?;

        Ok(())
    }

    fn create_pnpm_workspace_project(&self, config: &TestProject) -> Result<()> {
        let workspace_root = self.workspace_root.as_ref().unwrap();
        fs::create_dir_all(workspace_root)?;
        fs::create_dir_all(&self.project_path)?;

        // Create pnpm-workspace.yaml
        let pnpm_workspace = format!(
            r#"packages:
  - '{}'
"#,
            config.name
        );

        fs::write(workspace_root.join("pnpm-workspace.yaml"), pnpm_workspace)?;

        // Create workspace root package.json
        let workspace_package_json = format!(
            r#"{{
  "name": "test-pnpm-workspace",
  "version": "1.0.0",
  "private": true,
  "dependencies": {{
{}
  }}
}}"#,
            self.format_dependencies(&config.dependencies)
        );

        fs::write(workspace_root.join("package.json"), workspace_package_json)?;

        // Create project package.json
        let project_package_json = self.generate_package_json(config)?;
        fs::write(self.project_path.join("package.json"), project_package_json)?;

        // Create project files (similar to workspace but with pnpm-specific messaging)
        let index_js = r#"console.log("Hello from pnpm workspace project!");
console.log("Node version:", process.version);

// Test pnpm workspace dependencies
try {
    const deps = require('./package.json').dependencies || {};
    console.log("Dependencies:", Object.keys(deps));
    
    // Test specific dependencies
    if (deps['adm-zip']) {
        const AdmZip = require('adm-zip');
        console.log("Successfully loaded adm-zip from pnpm workspace:", typeof AdmZip);
        
        // Test basic functionality
        const zip = new AdmZip();
        zip.addFile("test.txt", Buffer.from("pnpm workspace test content"));
        const entries = zip.getEntries();
        console.log("Zip entries count:", entries.length);
        console.log("PNPM_WORKSPACE_DEPENDENCY_TEST_PASSED");
    }
} catch (e) {
    console.error("Pnpm workspace dependency test failed:", e.message);
    console.log("PNPM_WORKSPACE_DEPENDENCY_TEST_FAILED");
}

console.log("Pnpm workspace project test completed!");
process.exit(0);"#;

        fs::write(self.project_path.join("index.js"), index_js)?;

        Ok(())
    }

    fn generate_package_json(&self, config: &TestProject) -> Result<String> {
        let deps = self.format_dependencies(&config.dependencies);
        let dev_deps = self.format_dependencies(&config.dev_dependencies);

        let package_json = format!(
            r#"{{
  "name": "{}",
  "version": "1.0.0",
  "main": "index.js",
  "scripts": {{
    "start": "node index.js"
  }}{}{}
}}"#,
            config.name,
            if deps.is_empty() {
                String::new()
            } else {
                format!(",\n  \"dependencies\": {{\n{deps}\n  }}")
            },
            if dev_deps.is_empty() {
                String::new()
            } else {
                format!(",\n  \"devDependencies\": {{\n{dev_deps}\n  }}")
            }
        );

        Ok(package_json)
    }

    fn format_dependencies(&self, deps: &[(String, String)]) -> String {
        deps.iter()
            .map(|(name, version)| format!("    \"{name}\": \"{version}\""))
            .collect::<Vec<_>>()
            .join(",\n")
    }
}

/// Bundler test helper for running the bundler in tests
pub struct BundlerTestHelper;

impl BundlerTestHelper {
    /// Get the path to the banderole binary
    pub fn get_bundler_path() -> Result<PathBuf> {
        let target_dir = std::env::current_dir()?.join("target");
        let bundler_path = if cfg!(windows) {
            target_dir.join("debug/banderole.exe")
        } else {
            target_dir.join("debug/banderole")
        };

        if !bundler_path.exists() {
            // Build the bundler if it doesn't exist
            println!("Building banderole...");
            let output = Command::new("cargo").args(["build"]).output()?;

            if !output.status.success() {
                anyhow::bail!(
                    "Failed to build banderole: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }

        Ok(bundler_path)
    }

    /// Bundle a project and return the path to the created executable
    pub fn bundle_project(
        project_path: &Path,
        output_dir: &Path,
        custom_name: Option<&str>,
    ) -> Result<PathBuf> {
        Self::bundle_project_with_compression(project_path, output_dir, custom_name, true)
    }

    /// Bundle a project with compression control and return the path to the created executable
    pub fn bundle_project_with_compression(
        project_path: &Path,
        output_dir: &Path,
        custom_name: Option<&str>,
        enable_compression: bool,
    ) -> Result<PathBuf> {
        let bundler_path = Self::get_bundler_path()?;

        let mut cmd = Command::new(&bundler_path);
        cmd.args(["bundle", project_path.to_str().unwrap()])
            .current_dir(output_dir);

        if let Some(name) = custom_name {
            cmd.args(["--name", name]);
        }

        if !enable_compression {
            cmd.arg("--no-compression");
        }

        let bundle_output = Self::run_with_timeout(&mut cmd, Duration::from_secs(300))?;

        if !bundle_output.status.success() {
            anyhow::bail!(
                "Bundle command failed:\nStdout: {}\nStderr: {}",
                String::from_utf8_lossy(&bundle_output.stdout),
                String::from_utf8_lossy(&bundle_output.stderr)
            );
        }

        // Debug: Print bundler output
        println!(
            "Bundler stdout: {}",
            String::from_utf8_lossy(&bundle_output.stdout)
        );
        if !bundle_output.stderr.is_empty() {
            println!(
                "Bundler stderr: {}",
                String::from_utf8_lossy(&bundle_output.stderr)
            );
        }

        // Find the created executable (normalize Windows extension rules)
        let executable_name = custom_name.unwrap_or("test-project");
        let mut candidate_names: Vec<PathBuf> = Vec::new();
        if cfg!(windows) {
            // Prefer explicit .exe
            candidate_names.push(output_dir.join(format!("{executable_name}.exe")));
            // If name provided might already include .exe
            candidate_names.push(output_dir.join(executable_name));
            // Collision-avoidance fallback
            candidate_names.push(output_dir.join(format!("{executable_name}-bundle.exe")));
            candidate_names.push(output_dir.join(format!("{executable_name}-bundle")));
        } else {
            candidate_names.push(output_dir.join(executable_name));
            candidate_names.push(output_dir.join(format!("{executable_name}-bundle")));
        }
        let executable_path = candidate_names
            .into_iter()
            .find(|p| p.exists() && p.is_file())
            .ok_or_else(|| {
                // List directory contents for debugging
                let dir_contents = fs::read_dir(output_dir)
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok())
                            .map(|entry| {
                                let path = entry.path();
                                let metadata = fs::metadata(&path).ok();
                                format!(
                                    "{} (size: {}, is_file: {})",
                                    entry.file_name().to_string_lossy(),
                                    metadata.as_ref().map(|m| m.len()).unwrap_or(0),
                                    metadata.as_ref().map(|m| m.is_file()).unwrap_or(false)
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_else(|e| vec![format!("Error reading directory: {}", e)]);
                anyhow::anyhow!(
                    "Executable was not created under {} with expected names. Output directory contents: {:?}",
                    output_dir.display(),
                    dir_contents
                )
            })?;

        if !executable_path.exists() {
            // List directory contents for debugging
            let dir_contents = fs::read_dir(output_dir)
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .map(|entry| {
                            let path = entry.path();
                            let metadata = fs::metadata(&path).ok();
                            format!(
                                "{} (size: {}, is_file: {})",
                                entry.file_name().to_string_lossy(),
                                metadata.as_ref().map(|m| m.len()).unwrap_or(0),
                                metadata.as_ref().map(|m| m.is_file()).unwrap_or(false)
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|e| vec![format!("Error reading directory: {}", e)]);

            anyhow::bail!(
                "Executable was not created at {}\nExpected name: {}\nOutput directory: {}\nOutput directory contents: {:?}",
                executable_path.display(),
                executable_name,
                output_dir.display(),
                dir_contents
            );
        }

        Ok(executable_path)
    }

    /// Run an executable and return the output
    pub fn run_executable(
        executable_path: &Path,
        args: &[&str],
        env_vars: &[(&str, &str)],
    ) -> Result<std::process::Output> {
        // Verify executable exists and is accessible
        if !executable_path.exists() {
            let parent_contents = executable_path
                .parent()
                .and_then(|p| fs::read_dir(p).ok())
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| vec!["Could not read directory".to_string()]);

            anyhow::bail!(
                "Executable does not exist at path: {}\nParent directory exists: {}\nParent directory contents: {:?}",
                executable_path.display(),
                executable_path.parent().map(|p| p.exists()).unwrap_or(false),
                parent_contents
            );
        }

        if let Ok(metadata) = fs::metadata(executable_path) {
            if !metadata.is_file() {
                anyhow::bail!(
                    "Path exists but is not a file: {} (is_dir: {})",
                    executable_path.display(),
                    metadata.is_dir()
                );
            }
        } else {
            anyhow::bail!(
                "Cannot read metadata for executable: {}",
                executable_path.display()
            );
        }

        // Make executable on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::metadata(executable_path)?.permissions();
            let mut perms = perms.clone();
            perms.set_mode(0o755);
            fs::set_permissions(executable_path, perms)?;
        }

        // Build command to run the executable.
        #[cfg(windows)]
        let (exec_to_run, work_dir, _run_guard) = {
            // Copy to a unique name in the same directory as the original to avoid policy issues with %TEMP%
            let parent = executable_path.parent().ok_or_else(|| {
                anyhow::anyhow!("Executable has no parent: {}", executable_path.display())
            })?;
            let run_dir = TempDir::new_in(parent).unwrap_or_else(|_| TempDir::new().unwrap());
            let mut base = executable_path
                .file_name()
                .map(|s| s.to_os_string())
                .unwrap_or_else(|| "app.exe".into());
            if Path::new(&base).extension().is_none() {
                base.push(".exe");
            }
            let candidate = run_dir.path().join(&base);
            std::fs::copy(executable_path, &candidate).with_context(|| {
                format!(
                    "Failed to copy executable to run dir: {} -> {}",
                    executable_path.display(),
                    candidate.display()
                )
            })?;
            std::thread::sleep(std::time::Duration::from_millis(50));
            if !candidate.exists() {
                anyhow::bail!(
                    "Run executable not found after copy: {}",
                    candidate.display()
                );
            }
            (candidate, parent.to_path_buf(), run_dir)
        };

        #[cfg(not(windows))]
        let (exec_to_run, work_dir) = (
            executable_path.to_path_buf(),
            executable_path.parent().unwrap().to_path_buf(),
        );

        println!("Executing: {} with args: {:?}", exec_to_run.display(), args);

        // Build a verbatim Windows path to avoid MAX_PATH and normalization issues
        #[cfg(windows)]
        let exec_for_spawn = {
            use std::ffi::OsString;
            let abs = exec_to_run
                .canonicalize()
                .unwrap_or_else(|_| exec_to_run.clone());
            let mut s: OsString = OsString::from(r"\\?\");
            s.push(&abs);
            s
        };
        #[cfg(not(windows))]
        let exec_for_spawn = exec_to_run.as_os_str().to_os_string();

        // First try direct spawn
        let direct = {
            let mut cmd = Command::new(&exec_for_spawn);
            cmd.args(args);
            for (key, value) in env_vars {
                cmd.env(key, value);
            }
            cmd.current_dir(&work_dir).output()
        };

        // If NotFound on Windows, retry using the copied executable directly with verbatim prefix; else cmd /C
        #[cfg(windows)]
        let output = match direct {
            Ok(o) => Ok(o),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                use std::ffi::OsString;
                let abs = exec_to_run
                    .canonicalize()
                    .unwrap_or_else(|_| exec_to_run.clone());
                let mut verbatim: OsString = OsString::from(r"\\?\");
                verbatim.push(&abs);
                let mut cmd = Command::new(&verbatim);
                cmd.args(args).current_dir(&work_dir);
                for (key, value) in env_vars {
                    cmd.env(key, value);
                }
                match cmd.output() {
                    Ok(o2) => Ok(o2),
                    Err(e2) if e2.kind() == std::io::ErrorKind::NotFound => {
                        // Fallback to cmd /C with quoting
                        fn quote(s: &str) -> String {
                            let mut out = String::from("\"");
                            out.push_str(&s.replace('"', "\\\""));
                            out.push('"');
                            out
                        }
                        let exe_str = exec_to_run.display().to_string();
                        let mut cmdline = quote(&exe_str);
                        for a in args {
                            cmdline.push(' ');
                            cmdline.push_str(&quote(a));
                        }
                        let mut c2 = Command::new("cmd");
                        c2.args(["/C", &cmdline]).current_dir(&work_dir);
                        for (key, value) in env_vars {
                            c2.env(key, value);
                        }
                        c2.output()
                    }
                    Err(e2) => Err(e2),
                }
            }
            Err(e) => Err(e),
        };

        #[cfg(not(windows))]
        let output = direct;

        let output = output.with_context(|| {
            format!(
                "Failed to execute command: {}\nArgs: {:?}\nEnv vars: {:?}\nWorking directory: {:?}",
                exec_to_run.display(),
                args,
                env_vars,
                &work_dir
            )
        })?;
        Ok(output)
    }

    /// Run a command with a timeout
    pub fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<std::process::Output> {
        use std::sync::mpsc;
        use std::thread;

        let child = cmd.spawn()?;
        let (tx, rx) = mpsc::channel();

        // Spawn a thread to wait for the process
        let child_id = child.id();
        thread::spawn(move || {
            let result = child.wait_with_output();
            let _ = tx.send(result);
        });

        // Wait for either completion or timeout
        match rx.recv_timeout(timeout) {
            Ok(result) => result.map_err(|e| anyhow::anyhow!("Command execution failed: {}", e)),
            Err(_) => {
                // Timeout occurred, kill the process
                if cfg!(unix) {
                    let _ = std::process::Command::new("kill")
                        .args(["-9", &child_id.to_string()])
                        .output();
                } else if cfg!(windows) {
                    let _ = std::process::Command::new("taskkill")
                        .args(["/F", "/PID", &child_id.to_string()])
                        .output();
                }

                anyhow::bail!("Command timed out after {:?}", timeout)
            }
        }
    }
}

/// Test cache management utilities
pub struct TestCacheManager;

impl TestCacheManager {
    /// Clear application cache for testing
    pub fn clear_application_cache() -> Result<()> {
        // Determine cache directory based on platform
        let cache_dir = if cfg!(windows) {
            if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
                std::path::PathBuf::from(local_app_data).join("banderole")
            } else {
                return Ok(()); // Can't determine cache dir, skip cleanup
            }
        } else if let Some(xdg_cache) = std::env::var_os("XDG_CACHE_HOME") {
            std::path::PathBuf::from(xdg_cache).join("banderole")
        } else if let Some(home) = std::env::var_os("HOME") {
            std::path::PathBuf::from(home)
                .join(".cache")
                .join("banderole")
        } else {
            std::path::PathBuf::from("/tmp").join("banderole-cache")
        };

        if cache_dir.exists() {
            println!("Clearing application cache at: {}", cache_dir.display());

            // Only remove application cache directories, not the Node.js cache
            if let Ok(entries) = std::fs::read_dir(&cache_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                        // Only remove directories that look like UUIDs (application cache)
                        // Keep "node" directory (Node.js binaries cache)
                        if dir_name != "node" && dir_name.len() > 10 {
                            if let Err(e) = std::fs::remove_dir_all(&path) {
                                println!(
                                    "Warning: Failed to remove cache directory {}: {}",
                                    path.display(),
                                    e
                                );
                            } else {
                                println!("Removed cache directory: {}", path.display());
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Assertion helpers for test verification
pub struct TestAssertions;

impl TestAssertions {
    /// Assert that the bundled executable runs successfully and produces expected output
    pub fn assert_executable_works(
        executable_path: &Path,
        expected_outputs: &[&str],
        env_vars: &[(&str, &str)],
        args: &[&str],
    ) -> Result<()> {
        let output = BundlerTestHelper::run_executable(executable_path, args, env_vars)?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            anyhow::bail!(
                "Executable failed with exit code {:?}.\nStdout: {}\nStderr: {}",
                output.status.code(),
                stdout,
                stderr
            );
        }

        for expected in expected_outputs {
            if !stdout.contains(expected) {
                anyhow::bail!(
                    "Expected output '{}' not found in stdout:\n{}",
                    expected,
                    stdout
                );
            }
        }

        Ok(())
    }

    /// Assert that dependency tests pass in the bundled executable
    pub fn assert_dependency_test_passes(executable_path: &Path, test_marker: &str) -> Result<()> {
        let output = BundlerTestHelper::run_executable(executable_path, &[], &[])?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            anyhow::bail!(
                "Dependency test executable failed with exit code {:?}.\nStdout: {}\nStderr: {}",
                output.status.code(),
                stdout,
                stderr
            );
        }

        if !stdout.contains(test_marker) {
            anyhow::bail!(
                "Dependency test failed - marker '{}' not found in output:\n{}",
                test_marker,
                stdout
            );
        }

        Ok(())
    }
}
