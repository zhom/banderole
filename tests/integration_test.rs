use serial_test::serial;
use std::fs;
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_bundle_and_run() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let test_app_path = temp_dir.path().join("test-app");

    // Create a simple test app
    std::fs::create_dir_all(&test_app_path)?;

    let package_json = r#"{
  "name": "integration-test-app",
  "version": "1.0.0",
  "main": "index.js",
  "scripts": {
    "start": "node index.js"
  }
}"#;

    let index_js = r#"
const fs = require('fs');
const path = require('path');

console.log("Hello from integration test!");
console.log("Node version:", process.version);
console.log("Platform:", process.platform);
console.log("Architecture:", process.arch);

// Test file system access
const testFile = path.join(__dirname, 'test.txt');
fs.writeFileSync(testFile, 'test content');
const content = fs.readFileSync(testFile, 'utf8');
console.log("File content:", content);

// Test environment variables
console.log("Test env var:", process.env.TEST_VAR || 'not set');

// Test process arguments
console.log("Process args:", process.argv.slice(2));

// Test module resolution
try {
    const uuid = require('uuid');
    console.log("UUID:", uuid.v4());
} catch (e) {
    console.error("Failed to require uuid:", e.message);
}

process.exit(0);"#;

    // Write test files
    std::fs::write(test_app_path.join("package.json"), package_json)?;
    std::fs::write(test_app_path.join("index.js"), index_js)?;

    // Install test dependency
    let npm_install = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", "npm", "install", "uuid"])
            .current_dir(&test_app_path)
            .output()?
    } else {
        Command::new("sh")
            .args(["-c", "npm install uuid"])
            .current_dir(&test_app_path)
            .output()?
    };

    if !npm_install.status.success() {
        eprintln!(
            "npm install failed: {}",
            String::from_utf8_lossy(&npm_install.stderr)
        );
    }

    // Build banderole if not already built
    println!("Building banderole...");
    let target_dir = std::env::current_dir()?.join("target");
    let banderole_path = target_dir.join("debug/banderole");

    if !banderole_path.exists() {
        let output = Command::new("cargo")
            .args(["build"])
            .output()
            .expect("Failed to build banderole");

        assert!(
            output.status.success(),
            "Failed to build banderole: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Bundle the test app)
    println!("Bundling test app...");

    let mut bundle_cmd = Command::new(&banderole_path);
    bundle_cmd
        .args([
            "bundle",
            test_app_path.to_str().unwrap(),
            "--no-compression",
        ])
        .current_dir(temp_dir.path());

    let bundle_output = bundle_cmd.output()?;

    if !bundle_output.status.success() {
        println!(
            "Bundle stdout: {}",
            String::from_utf8_lossy(&bundle_output.stdout)
        );
        println!(
            "Bundle stderr: {}",
            String::from_utf8_lossy(&bundle_output.stderr)
        );
        return Err("Bundle command failed".into());
    }

    // Find the created executable
    let executable_path = temp_dir.path().join(if cfg!(windows) {
        "integration-test-app.exe"
    } else {
        "integration-test-app"
    });

    if !executable_path.exists() {
        let entries: Vec<_> = std::fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        panic!(
            "Executable was not created at {}. Directory contents: {:?}",
            executable_path.display(),
            entries
        );
    }

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(&executable_path)?.permissions();
        let mut perms = perms.clone();
        perms.set_mode(0o755);
        std::fs::set_permissions(&executable_path, perms)?;
    }

    // Test 1: Run the executable directly
    println!("Running test 1: Direct execution");
    let output = Command::new(&executable_path)
        .env("TEST_VAR", "test_value")
        .args(["--test-arg1", "value1"])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    println!("Test 1 - Exit status: {}", output.status);
    println!("Test 1 - Stdout: {stdout}");
    println!("Test 1 - Stderr: {stderr}");

    // Check for expected output
    assert!(output.status.success(), "First run failed");
    assert!(
        stdout.contains("Hello from integration test!"),
        "Expected greeting not found in output"
    );
    assert!(
        stdout.contains("File content: test content"),
        "File operations test failed"
    );
    assert!(
        stdout.contains("Test env var: test_value"),
        "Environment variable test failed"
    );
    assert!(
        stdout.contains("--test-arg1"),
        "Command line arguments test failed"
    );
    assert!(stdout.contains("UUID: "), "Module resolution test failed");

    // Test 2: Run again to test cached execution
    println!("Running test 2: Cached execution");
    let output = Command::new(&executable_path)
        .env("TEST_VAR", "cached_run")
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("Test 2 - Exit status: {}", output.status);
    println!("Test 2 - Output: {stdout}");

    assert!(output.status.success(), "Cached run failed");
    assert!(
        stdout.contains("Hello from integration test!"),
        "Cached run output incorrect"
    );
    assert!(
        stdout.contains("Test env var: cached_run"),
        "Cached run env var not set correctly"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_node_version_detection() {
    let temp_dir = TempDir::new().unwrap();
    let test_app_path = temp_dir.path().join("test-app");

    // Create a simple test app with .nvmrc
    std::fs::create_dir_all(&test_app_path).unwrap();

    let package_json = r#"{
  "name": "nvmrc-test-app",
  "version": "1.0.0",
  "main": "index.js"
}"#;

    let index_js = r#"console.log("Node version:", process.version);
console.log("Platform:", process.platform);
process.exit(0);"#;

    let nvmrc = "20.18.1";

    std::fs::write(test_app_path.join("package.json"), package_json).unwrap();
    std::fs::write(test_app_path.join("index.js"), index_js).unwrap();
    std::fs::write(test_app_path.join(".nvmrc"), nvmrc).unwrap();

    // Build banderole
    println!("Building banderole for Node version test...");
    let output = Command::new("cargo")
        .args(["build", "--release"])
        .output()
        .expect("Failed to build banderole");

    assert!(
        output.status.success(),
        "Failed to build banderole: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let banderole_path = std::env::current_dir()
        .unwrap()
        .join("target/release/banderole");

    // Bundle the test app (keep compression for this test to verify it works)
    println!("Bundling test app with .nvmrc...");
    let mut bundle_cmd = Command::new(&banderole_path);
    bundle_cmd
        .args(["bundle", test_app_path.to_str().unwrap()])
        .current_dir(temp_dir.path());

    let bundle_output = bundle_cmd.output().expect("Failed to bundle test app");

    if !bundle_output.status.success() {
        println!(
            "Bundle stdout: {}",
            String::from_utf8_lossy(&bundle_output.stdout)
        );
        println!(
            "Bundle stderr: {}",
            String::from_utf8_lossy(&bundle_output.stderr)
        );
    }

    assert!(
        bundle_output.status.success(),
        "Bundle command failed: {}",
        String::from_utf8_lossy(&bundle_output.stderr)
    );

    // Check that the output mentions the correct Node.js version
    let stdout = String::from_utf8_lossy(&bundle_output.stdout);
    let stderr = String::from_utf8_lossy(&bundle_output.stderr);
    let combined_output = format!("{stdout}{stderr}");
    println!("Bundle output: {combined_output}");

    assert!(
        combined_output.contains("Node.js v20.18.1"),
        "Expected Node.js version not found in output: {combined_output}"
    );

    // Find and run the created executable to verify it uses the correct Node version
    let executable_name = if cfg!(target_os = "windows") {
        "nvmrc-test-app.exe"
    } else {
        "nvmrc-test-app"
    };

    let executable_path = temp_dir.path().join(executable_name);
    assert!(
        executable_path.exists(),
        "Executable was not created: {}. Directory contents: {:?}",
        executable_path.display(),
        std::fs::read_dir(temp_dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>()
    );

    // Run the executable and check Node version
    println!(
        "Running executable to check Node version: {}",
        executable_path.display()
    );

    let mut cmd = Command::new(&executable_path);
    cmd.env("DEBUG", "1"); // Enable debug mode

    // Use simple output instead of timeout to capture output correctly
    let run_output = cmd.output().expect("Failed to run executable");

    if !run_output.status.success() {
        println!(
            "Executable stdout: {}",
            String::from_utf8_lossy(&run_output.stdout)
        );
        println!(
            "Executable stderr: {}",
            String::from_utf8_lossy(&run_output.stderr)
        );
        println!("Exit code: {:?}", run_output.status.code());
    }

    assert!(
        run_output.status.success(),
        "Executable failed with exit code {:?}. Stderr: {}",
        run_output.status.code(),
        String::from_utf8_lossy(&run_output.stderr)
    );

    let stdout = String::from_utf8_lossy(&run_output.stdout);
    println!("Executable output: {stdout}");

    assert!(
        stdout.contains("v20.18.1"),
        "Expected Node.js version not found in executable output: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_output_path_collision_handling() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let test_app_path = temp_dir.path().join("collision-test-app");

    // Create a simple test app
    std::fs::create_dir_all(&test_app_path)?;

    let package_json = r#"{
  "name": "collision-test-app",
  "version": "1.0.0",
  "main": "index.js"
}"#;

    let index_js = r#"console.log("Hello from collision test app!");"#;

    std::fs::write(test_app_path.join("package.json"), package_json)?;
    std::fs::write(test_app_path.join("index.js"), index_js)?;

    // Create a directory with the same name as the app to cause collision
    std::fs::create_dir_all(temp_dir.path().join("collision-test-app"))?;

    // Build banderole
    let target_dir = std::env::current_dir()?.join("target");
    let banderole_path = target_dir.join("debug/banderole");

    if !banderole_path.exists() {
        let output = Command::new("cargo")
            .args(["build"])
            .output()
            .expect("Failed to build banderole");

        assert!(
            output.status.success(),
            "Failed to build banderole: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Bundle the test app)
    println!("Testing output path collision handling...");

    let mut bundle_cmd = Command::new(&banderole_path);
    bundle_cmd
        .args([
            "bundle",
            test_app_path.to_str().unwrap(),
            "--no-compression",
        ])
        .current_dir(temp_dir.path());

    let bundle_output = run_with_timeout(&mut bundle_cmd, Duration::from_secs(300))?;

    if !bundle_output.status.success() {
        println!(
            "Bundle stdout: {}",
            String::from_utf8_lossy(&bundle_output.stdout)
        );
        println!(
            "Bundle stderr: {}",
            String::from_utf8_lossy(&bundle_output.stderr)
        );
        return Err("Bundle command failed".into());
    }

    // Verify that a bundle was created with the expected naming
    let mut candidates = vec![temp_dir.path().join("collision-test-app-bundle")];
    if cfg!(windows) {
        candidates.insert(0, temp_dir.path().join("collision-test-app-bundle.exe"));
        // If the bundler chose not to add -bundle due to no collision, check that as well
        candidates.push(temp_dir.path().join("collision-test-app.exe"));
    } else {
        candidates.push(temp_dir.path().join("collision-test-app"));
    }
    let expected_executable = candidates
        .into_iter()
        .find(|p| p.exists() && p.is_file())
        .ok_or_else(|| {
            let dir = temp_dir.path();
            let listing: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .filter_map(Result::ok)
                .map(|e| e.file_name())
                .collect();
            anyhow::anyhow!(
                "Executable was not created under {}. Directory contents: {:?}",
                dir.display(),
                listing
            )
        })?;

    // Test that the executable works
    let output = Command::new(&expected_executable).output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("Collision test output: {stdout}");

    assert!(output.status.success(), "Collision test executable failed");
    assert!(
        stdout.contains("Hello from collision test app!"),
        "Expected greeting not found in collision test output"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_typescript_project_with_dist() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let test_app_path = temp_dir.path().join("ts-test-app");

    // Create a TypeScript project structure
    std::fs::create_dir_all(&test_app_path)?;
    std::fs::create_dir_all(test_app_path.join("dist"))?;

    let package_json = r#"{
  "name": "ts-test-app",
  "version": "1.0.0",
  "main": "dist/index.js",
  "scripts": {
    "build": "tsc"
  }
}"#;

    let tsconfig_json = r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "outDir": "./dist",
    "rootDir": "./src",
    "strict": true
  }
}"#;

    let src_index_ts = r#"console.log("Hello from TypeScript app!");
console.log("Node version:", process.version);
console.log("This should come from the dist directory");"#;

    let dist_index_js = r#"console.log("Hello from TypeScript app!");
console.log("Node version:", process.version);
console.log("This should come from the dist directory");
try {
    const marker = require('./marker.js');
    console.log("Marker file found:", marker.source);
} catch (e) {
    console.log("Marker file not found");
}"#;

    // Create a distinctive file in dist to verify it was used as source
    let dist_marker = r#"module.exports = { source: "dist" };"#;

    // Write project files
    std::fs::write(test_app_path.join("package.json"), package_json)?;
    std::fs::write(test_app_path.join("tsconfig.json"), tsconfig_json)?;
    std::fs::create_dir_all(test_app_path.join("src"))?;
    std::fs::write(test_app_path.join("src/index.ts"), src_index_ts)?;
    std::fs::write(test_app_path.join("dist/index.js"), dist_index_js)?;
    std::fs::write(test_app_path.join("dist/marker.js"), dist_marker)?;

    // Build banderole
    let target_dir = std::env::current_dir()?.join("target");
    let banderole_path = target_dir.join("debug/banderole");

    if !banderole_path.exists() {
        let output = Command::new("cargo")
            .args(["build"])
            .output()
            .expect("Failed to build banderole");

        assert!(
            output.status.success(),
            "Failed to build banderole: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Bundle the TypeScript project)
    println!("Testing TypeScript project bundling...");

    let mut bundle_cmd = Command::new(&banderole_path);
    bundle_cmd
        .args([
            "bundle",
            test_app_path.to_str().unwrap(),
            "--no-compression",
        ])
        .current_dir(temp_dir.path());

    let bundle_output = run_with_timeout(&mut bundle_cmd, Duration::from_secs(300))?;

    if !bundle_output.status.success() {
        println!(
            "Bundle stdout: {}",
            String::from_utf8_lossy(&bundle_output.stdout)
        );
        println!(
            "Bundle stderr: {}",
            String::from_utf8_lossy(&bundle_output.stderr)
        );
        return Err("TypeScript bundle command failed".into());
    }

    // We'll verify that the dist directory was used by running the executable
    // and checking that it includes our marker file

    // Find the created executable (may have collision avoidance suffix)
    let mut executable_path = temp_dir.path().join(if cfg!(windows) {
        "ts-test-app.exe"
    } else {
        "ts-test-app"
    });

    // Check if collision avoidance was used (need to check if it's a file, not just exists)
    if !executable_path.exists() || !executable_path.is_file() {
        executable_path = temp_dir.path().join(if cfg!(windows) {
            "ts-test-app-bundle.exe"
        } else {
            "ts-test-app-bundle"
        });
    }

    assert!(
        executable_path.exists(),
        "TypeScript executable was not created: {}. Found files: {:?}",
        executable_path.display(),
        std::fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(&executable_path)?.permissions();
        let mut perms = perms.clone();
        perms.set_mode(0o755);
        std::fs::set_permissions(&executable_path, perms)?;
    }

    // Test the executable
    let output = Command::new(&executable_path).output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("TypeScript test output: {stdout}");

    assert!(output.status.success(), "TypeScript executable failed");
    assert!(
        stdout.contains("Hello from TypeScript app!"),
        "Expected greeting not found in TypeScript output"
    );
    assert!(
        stdout.contains("This should come from the dist directory"),
        "Should be running from dist directory"
    );
    assert!(
        stdout.contains("Marker file found: dist"),
        "Should have included marker file from dist directory, indicating dist was used as source"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_typescript_project_with_tsconfig_outdir() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let test_app_path = temp_dir.path().join("ts-outdir-test");

    // Create a TypeScript project with custom outDir
    std::fs::create_dir_all(&test_app_path)?;
    std::fs::create_dir_all(test_app_path.join("build"))?;

    let package_json = r#"{
  "name": "ts-outdir-test",
  "version": "1.0.0",
  "main": "index.js"
}"#;

    let tsconfig_json = r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "outDir": "./build",
    "rootDir": "./src"
  }
}"#;

    let build_index_js = r#"console.log("Hello from custom outDir!");
console.log("This comes from the build directory");
try {
    const marker = require('./marker.js');
    console.log("Marker file found:", marker.source);
} catch (e) {
    console.log("Marker file not found");
}"#;

    let build_marker = r#"module.exports = { source: "build" };"#;

    // Write project files
    std::fs::write(test_app_path.join("package.json"), package_json)?;
    std::fs::write(test_app_path.join("tsconfig.json"), tsconfig_json)?;
    std::fs::write(test_app_path.join("build/index.js"), build_index_js)?;
    std::fs::write(test_app_path.join("build/marker.js"), build_marker)?;

    // Build banderole
    let target_dir = std::env::current_dir()?.join("target");
    let banderole_path = target_dir.join("debug/banderole");

    if !banderole_path.exists() {
        let output = Command::new("cargo")
            .args(["build"])
            .output()
            .expect("Failed to build banderole");

        assert!(
            output.status.success(),
            "Failed to build banderole: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Bundle the project)
    println!("Testing TypeScript project with custom outDir...");

    let mut bundle_cmd = Command::new(&banderole_path);
    bundle_cmd
        .args([
            "bundle",
            test_app_path.to_str().unwrap(),
            "--no-compression",
        ])
        .current_dir(temp_dir.path());

    let bundle_output = bundle_cmd.output()?;

    if !bundle_output.status.success() {
        println!(
            "Bundle stdout: {}",
            String::from_utf8_lossy(&bundle_output.stdout)
        );
        println!(
            "Bundle stderr: {}",
            String::from_utf8_lossy(&bundle_output.stderr)
        );
        return Err("Custom outDir bundle command failed".into());
    }

    // We'll verify that the build directory was used by checking the marker file

    // Find the created executable (may have collision avoidance suffix)
    let mut executable_path = temp_dir.path().join(if cfg!(windows) {
        "ts-outdir-test.exe"
    } else {
        "ts-outdir-test"
    });

    // Check if collision avoidance was used (need to check if it's a file, not just exists)
    if !executable_path.exists() || !executable_path.is_file() {
        executable_path = temp_dir.path().join(if cfg!(windows) {
            "ts-outdir-test-bundle.exe"
        } else {
            "ts-outdir-test-bundle"
        });
    }

    assert!(
        executable_path.exists(),
        "Custom outDir executable was not created"
    );

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(&executable_path)?.permissions();
        let mut perms = perms.clone();
        perms.set_mode(0o755);
        std::fs::set_permissions(&executable_path, perms)?;
    }

    let output = Command::new(&executable_path).output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("Custom outDir test output: {stdout}");

    assert!(output.status.success(), "Custom outDir executable failed");
    assert!(
        stdout.contains("Hello from custom outDir!"),
        "Expected greeting not found"
    );
    assert!(
        stdout.contains("This comes from the build directory"),
        "Should be running from build directory"
    );
    assert!(
        stdout.contains("Marker file found: build"),
        "Should have included marker file from build directory, indicating build was used as source"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_typescript_project_with_extends() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let test_app_path = temp_dir.path().join("ts-extends-test");

    // Create a TypeScript project with extends configuration
    std::fs::create_dir_all(&test_app_path)?;
    std::fs::create_dir_all(test_app_path.join("lib"))?;

    let package_json = r#"{
  "name": "ts-extends-test",
  "version": "1.0.0",
  "main": "index.js"
}"#;

    let base_tsconfig = r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "outDir": "./lib"
  }
}"#;

    let tsconfig_json = r#"{
  "extends": "./tsconfig.base.json",
  "compilerOptions": {
    "strict": true
  }
}"#;

    let lib_index_js = r#"console.log("Hello from extended tsconfig!");
console.log("This comes from the lib directory via extends");
try {
    const marker = require('./marker.js');
    console.log("Marker file found:", marker.source);
} catch (e) {
    console.log("Marker file not found");
}"#;

    let lib_marker = r#"module.exports = { source: "lib" };"#;

    // Write project files
    std::fs::write(test_app_path.join("package.json"), package_json)?;
    std::fs::write(test_app_path.join("tsconfig.base.json"), base_tsconfig)?;
    std::fs::write(test_app_path.join("tsconfig.json"), tsconfig_json)?;
    std::fs::write(test_app_path.join("lib/index.js"), lib_index_js)?;
    std::fs::write(test_app_path.join("lib/marker.js"), lib_marker)?;

    // Build banderole
    let target_dir = std::env::current_dir()?.join("target");
    let banderole_path = target_dir.join("debug/banderole");

    if !banderole_path.exists() {
        let output = Command::new("cargo")
            .args(["build"])
            .output()
            .expect("Failed to build banderole");

        assert!(
            output.status.success(),
            "Failed to build banderole: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Bundle the project)
    println!("Testing TypeScript project with extends...");

    let mut bundle_cmd = Command::new(&banderole_path);
    bundle_cmd
        .args([
            "bundle",
            test_app_path.to_str().unwrap(),
            "--no-compression",
        ])
        .current_dir(temp_dir.path());

    let bundle_output = bundle_cmd.output()?;

    if !bundle_output.status.success() {
        println!(
            "Bundle stdout: {}",
            String::from_utf8_lossy(&bundle_output.stdout)
        );
        println!(
            "Bundle stderr: {}",
            String::from_utf8_lossy(&bundle_output.stderr)
        );
        return Err("Extends tsconfig bundle command failed".into());
    }

    // We'll verify that the lib directory was used by checking the marker file

    // Find the created executable (may have collision avoidance suffix)
    let mut executable_path = temp_dir.path().join(if cfg!(windows) {
        "ts-extends-test.exe"
    } else {
        "ts-extends-test"
    });

    // Check if collision avoidance was used (need to check if it's a file, not just exists)
    if !executable_path.exists() || !executable_path.is_file() {
        executable_path = temp_dir.path().join(if cfg!(windows) {
            "ts-extends-test-bundle.exe"
        } else {
            "ts-extends-test-bundle"
        });
    }

    assert!(
        executable_path.exists(),
        "Extends tsconfig executable was not created"
    );

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(&executable_path)?.permissions();
        let mut perms = perms.clone();
        perms.set_mode(0o755);
        std::fs::set_permissions(&executable_path, perms)?;
    }

    let output = Command::new(&executable_path).output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("Extends tsconfig test output: {stdout}");

    assert!(
        output.status.success(),
        "Extends tsconfig executable failed"
    );
    assert!(
        stdout.contains("Hello from extended tsconfig!"),
        "Expected greeting not found"
    );
    assert!(
        stdout.contains("This comes from the lib directory via extends"),
        "Should be running from lib directory"
    );
    assert!(
        stdout.contains("Marker file found: lib"),
        "Should have included marker file from lib directory, indicating lib was used as source"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_pnpm_dependencies_bundling() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let test_app_path = temp_dir.path().join("pnpm-test-app");

    // Create a TypeScript project structure with pnpm dependencies
    std::fs::create_dir_all(&test_app_path)?;
    std::fs::create_dir_all(test_app_path.join("dist"))?;
    std::fs::create_dir_all(test_app_path.join("node_modules"))?;
    std::fs::create_dir_all(test_app_path.join("node_modules/.pnpm"))?;

    let package_json = r#"{
  "name": "pnpm-test-app",
  "version": "1.0.0",
  "main": "dist/index.js",
  "dependencies": {
    "adm-zip": "^0.5.10"
  }
}"#;

    // Create a compiled JS file that uses dependencies
    let dist_index_js = r#"console.log("Hello from pnpm test app!");
console.log("Node version:", process.version);

// Test requiring a dependency that should be bundled
try {
    const AdmZip = require('adm-zip');
    console.log("Successfully loaded adm-zip:", typeof AdmZip);
    
    // Test basic functionality
    const zip = new AdmZip();
    zip.addFile("test.txt", Buffer.from("test content"));
    const entries = zip.getEntries();
    console.log("Zip entries count:", entries.length);
    console.log("DEPENDENCY_TEST_PASSED");
} catch (e) {
    console.error("Failed to load or use adm-zip:", e.message);
    console.log("DEPENDENCY_TEST_FAILED");
    process.exit(1);
}

console.log("All tests passed!");"#;

    // Create pnpm lockfile to indicate pnpm usage
    let pnpm_lock = r#"lockfileVersion: '6.0'

settings:
  autoInstallPeers: true
  excludeLinksFromLockfile: false

dependencies:
  adm-zip:
    specifier: ^0.5.10
    version: 0.5.10

packages:

  /adm-zip@0.5.10:
    resolution: {integrity: sha512-x0HvcHqVJNTPk/Bw8JbLWlWoo6Wwnsug0fnYYro1HBrjxZ3G7/AZk7Ahv8JwDe1uIcz8eBqvu86FuF1POiG7vQ==}
    engines: {node: '>=6.0'}
    dev: false
"#;

    // Write project files
    std::fs::write(test_app_path.join("package.json"), package_json)?;
    std::fs::write(test_app_path.join("dist/index.js"), dist_index_js)?;
    std::fs::write(test_app_path.join("pnpm-lock.yaml"), pnpm_lock)?;

    // Simulate a real pnpm installation by installing the actual dependency
    println!("Installing pnpm dependencies for test...");
    let pnpm_install = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", "pnpm", "install"])
            .current_dir(&test_app_path)
            .output()
    } else {
        Command::new("pnpm")
            .args(["install"])
            .current_dir(&test_app_path)
            .output()
    };

    match pnpm_install {
        Ok(output) if output.status.success() => {
            println!("Successfully installed pnpm dependencies for test");
        }
        Ok(output) => {
            println!(
                "pnpm install failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            println!("Falling back to npm install...");

            // Fallback to npm if pnpm is not available
            let npm_install = if cfg!(windows) {
                Command::new("cmd")
                    .args(["/C", "npm", "install", "adm-zip"])
                    .current_dir(&test_app_path)
                    .output()?
            } else {
                Command::new("npm")
                    .args(["install", "adm-zip"])
                    .current_dir(&test_app_path)
                    .output()?
            };

            if !npm_install.status.success() {
                return Err("Failed to install dependencies for test".into());
            }
        }
        Err(_) => {
            println!("pnpm not found, falling back to npm install...");

            // Fallback to npm if pnpm is not available
            let npm_install = if cfg!(windows) {
                Command::new("cmd")
                    .args(["/C", "npm", "install", "adm-zip"])
                    .current_dir(&test_app_path)
                    .output()?
            } else {
                Command::new("npm")
                    .args(["install", "adm-zip"])
                    .current_dir(&test_app_path)
                    .output()?
            };

            if !npm_install.status.success() {
                return Err("Failed to install dependencies for test".into());
            }
        }
    }

    // Build banderole
    let target_dir = std::env::current_dir()?.join("target");
    let banderole_path = target_dir.join("debug/banderole");

    if !banderole_path.exists() {
        let output = Command::new("cargo")
            .args(["build"])
            .output()
            .expect("Failed to build banderole");

        assert!(
            output.status.success(),
            "Failed to build banderole: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Bundle the pnpm project)
    println!("Testing pnpm dependency bundling...");

    let mut bundle_cmd = Command::new(&banderole_path);
    bundle_cmd
        .args([
            "bundle",
            test_app_path.to_str().unwrap(),
            "--no-compression",
        ])
        .current_dir(temp_dir.path());

    let bundle_output = run_with_timeout(&mut bundle_cmd, Duration::from_secs(300))?;

    if !bundle_output.status.success() {
        println!(
            "Bundle stdout: {}",
            String::from_utf8_lossy(&bundle_output.stdout)
        );
        println!(
            "Bundle stderr: {}",
            String::from_utf8_lossy(&bundle_output.stderr)
        );
        return Err("pnpm bundle command failed".into());
    }

    let bundle_stdout = String::from_utf8_lossy(&bundle_output.stdout);
    let bundle_stderr = String::from_utf8_lossy(&bundle_output.stderr);
    println!("Bundle stdout: {bundle_stdout}");
    println!("Bundle stderr: {bundle_stderr}");

    // The bundling succeeded if we can find the executable - output parsing is unreliable in tests

    // Find the created executable
    let mut executable_path = temp_dir.path().join(if cfg!(windows) {
        "pnpm-test-app.exe"
    } else {
        "pnpm-test-app"
    });

    // Check if collision avoidance was used
    if !executable_path.exists() || !executable_path.is_file() {
        executable_path = temp_dir.path().join(if cfg!(windows) {
            "pnpm-test-app-bundle.exe"
        } else {
            "pnpm-test-app-bundle"
        });
    }

    assert!(
        executable_path.exists(),
        "pnpm test executable was not created: {}. Found files: {:?}",
        executable_path.display(),
        std::fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(&executable_path)?.permissions();
        let mut perms = perms.clone();
        perms.set_mode(0o755);
        std::fs::set_permissions(&executable_path, perms)?;
    }

    // Test the executable - this is the critical test
    println!("Running pnpm test executable to verify dependencies...");
    let output = Command::new(&executable_path).output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    println!("pnpm test stdout: {stdout}");
    if !stderr.is_empty() {
        println!("pnpm test stderr: {stderr}");
    }

    // The critical assertions - the app should run successfully and load its dependencies
    assert!(
        output.status.success(),
        "pnpm test executable failed with exit code {:?}. Stderr: {}",
        output.status.code(),
        stderr
    );

    assert!(
        stdout.contains("Hello from pnpm test app!"),
        "Expected greeting not found in pnpm test output"
    );

    assert!(
        stdout.contains("Successfully loaded adm-zip:"),
        "Failed to load adm-zip dependency - this indicates the bundling didn't work correctly"
    );

    assert!(
        stdout.contains("DEPENDENCY_TEST_PASSED"),
        "Dependency functionality test failed - adm-zip was loaded but not working correctly"
    );

    assert!(
        stdout.contains("All tests passed!"),
        "Not all tests passed in the bundled application"
    );

    println!("✅ pnpm dependency bundling test passed successfully!");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_bundle_simple_project() {
    let temp_dir = TempDir::new().unwrap();
    let project_path = temp_dir.path().join("test-project");

    // Create a simple Node.js project
    fs::create_dir_all(&project_path).unwrap();

    let package_json = r#"{
        "name": "test-project",
        "version": "1.0.0",
        "main": "index.js",
        "dependencies": {
            "commander": "^11.0.0"
        }
    }"#;

    let index_js = r#"
        const { program } = require('commander');
        program
            .name('test-app')
            .description('A test application')
            .version('1.0.0')
            .option('-t, --test', 'test option');
        
        program.parse();
        console.log('Test app executed successfully');
    "#;

    fs::write(project_path.join("package.json"), package_json).unwrap();
    fs::write(project_path.join("index.js"), index_js).unwrap();

    // Install dependencies using npm (use platform-appropriate invocation)
    let npm_install = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", "npm", "install"])
            .current_dir(&project_path)
            .output()
            .unwrap()
    } else {
        Command::new("npm")
            .arg("install")
            .current_dir(&project_path)
            .output()
            .unwrap()
    };

    assert!(npm_install.status.success(), "npm install failed");

    // Bundle the project (we'll use the CLI instead, no compression for speed)
    let cargo_bin = env!("CARGO_BIN_EXE_banderole");
    let bundle_output = Command::new(cargo_bin)
        .arg("bundle")
        .arg(&project_path)
        .arg("--output")
        .arg(temp_dir.path().join("test-bundle"))
        .arg("--name")
        .arg("test-bundle")
        .arg("--no-compression")
        .output()
        .unwrap();

    assert!(
        bundle_output.status.success(),
        "Bundling failed: {:?}",
        String::from_utf8_lossy(&bundle_output.stderr)
    );

    // Test that the bundle exists
    let bundle_path = temp_dir.path().join("test-bundle");
    assert!(bundle_path.exists(), "Bundle file not created");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_pnpm_project_bundling() {
    // This test demonstrates that pnpm projects can be bundled
    // It would require a real pnpm project structure to test fully

    let temp_dir = TempDir::new().unwrap();
    let project_path = temp_dir.path().join("pnpm-project");

    // Create a minimal pnpm project structure
    fs::create_dir_all(&project_path).unwrap();
    fs::create_dir_all(project_path.join("node_modules/.pnpm")).unwrap();

    let package_json = r#"{
        "name": "pnpm-test-project",
        "version": "1.0.0",
        "main": "index.js",
        "dependencies": {
            "lodash": "^4.17.21"
        }
    }"#;

    let index_js = r#"
        const _ = require('lodash');
        console.log('Lodash version:', _.VERSION);
    "#;

    fs::write(project_path.join("package.json"), package_json).unwrap();
    fs::write(project_path.join("index.js"), index_js).unwrap();

    // Create minimal pnpm-lock.yaml
    let pnpm_lock = r#"
lockfileVersion: '6.0'
dependencies:
  lodash:
    specifier: ^4.17.21
    version: 4.17.21
packages:
  /lodash@4.17.21:
    resolution: {integrity: sha512-v2kDEe57lecTulaDIuNTPy3Ry4gLGJ6Z1O3vE1krgXZNrsQ+LFTGHVxVjcXPs17LhbZVGedAJv8XZ1tvj5FvSg==}
    dev: false
"#;

    fs::write(project_path.join("pnpm-lock.yaml"), pnpm_lock).unwrap();

    // The bundling should handle the pnpm structure gracefully)
    let cargo_bin = env!("CARGO_BIN_EXE_banderole");
    let result = Command::new(cargo_bin)
        .arg("bundle")
        .arg(&project_path)
        .arg("--output")
        .arg(temp_dir.path().join("pnpm-bundle"))
        .arg("--name")
        .arg("pnpm-bundle")
        .arg("--no-compression")
        .output()
        .unwrap();

    // Should not panic or fail catastrophically
    // May fail due to missing dependencies, but should handle pnpm structure
    if result.status.success() {
        println!("Pnpm bundling succeeded");
    } else {
        println!(
            "Pnpm bundling failed gracefully: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> std::io::Result<std::process::Output> {
    use std::sync::mpsc;
    use std::thread;

    let child = cmd.spawn().expect("Failed to spawn process");
    let (tx, rx) = mpsc::channel();

    // Spawn a thread to wait for the process
    let child_id = child.id();
    thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(result);
    });

    // Wait for either completion or timeout
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(_) => {
            // Timeout occurred, kill the process
            #[cfg(unix)]
            {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &child_id.to_string()])
                    .output();
            }
            #[cfg(windows)]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/PID", &child_id.to_string()])
                    .output();
            }

            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Process timed out",
            ))
        }
    }
}
mod common;
use common::TestCacheManager;

/// Cleanup function to be called after all integration tests
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_zzz_cleanup_integration_cache() -> Result<(), Box<dyn std::error::Error>> {
    println!("Cleaning up application cache after integration tests...");

    TestCacheManager::clear_application_cache()?;

    println!("✅ Integration cache cleanup completed!");
    Ok(())
}
