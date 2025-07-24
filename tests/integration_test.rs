use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test]
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
        eprintln!("npm install failed: {}", String::from_utf8_lossy(&npm_install.stderr));
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

    // Bundle the test app
    println!("Bundling test app...");
    
    let mut bundle_cmd = Command::new(&banderole_path);
    bundle_cmd
        .args(["bundle", test_app_path.to_str().unwrap()])
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

    // Find the created executable
    let executable_path = temp_dir.path().join(if cfg!(windows) {
        "integration-test-app-1.0.0-win32-x64.exe"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        "integration-test-app-1.0.0-darwin-arm64"
    } else if cfg!(target_os = "macos") {
        "integration-test-app-1.0.0-darwin-x64"
    } else if cfg!(target_arch = "aarch64") {
        "integration-test-app-1.0.0-linux-arm64"
    } else {
        "integration-test-app-1.0.0-linux-x64"
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
    println!("Test 1 - Stdout: {}", stdout);
    println!("Test 1 - Stderr: {}", stderr);

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
    assert!(
        stdout.contains("UUID: "),
        "Module resolution test failed"
    );

    // Test 2: Run again to test cached execution
    println!("Running test 2: Cached execution");
    let output = Command::new(&executable_path)
        .env("TEST_VAR", "cached_run")
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("Test 2 - Exit status: {}", output.status);
    println!("Test 2 - Output: {}", stdout);

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

#[test]
fn test_node_version_detection() {
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
        .args(&["build", "--release"])
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

    // Bundle the test app
    println!("Bundling test app with .nvmrc...");
    let mut bundle_cmd = Command::new(&banderole_path);
    bundle_cmd
        .args(&["bundle", test_app_path.to_str().unwrap()])
        .current_dir(temp_dir.path());

    let bundle_output = bundle_cmd.output()
        .expect("Failed to bundle test app");

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
    let combined_output = format!("{}{}", stdout, stderr);
    println!("Bundle output: {}", combined_output);

    assert!(
        combined_output.contains("Node.js v20.18.1"),
        "Expected Node.js version not found in output: {}",
        combined_output
    );

    // Find and run the created executable to verify it uses the correct Node version
    let executable_name = if cfg!(target_os = "windows") {
        "nvmrc-test-app-1.0.0-win32-x64.exe"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        "nvmrc-test-app-1.0.0-darwin-arm64"
    } else if cfg!(target_os = "macos") {
        "nvmrc-test-app-1.0.0-darwin-x64"
    } else if cfg!(target_arch = "aarch64") {
        "nvmrc-test-app-1.0.0-linux-arm64"
    } else {
        "nvmrc-test-app-1.0.0-linux-x64"
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
    let run_output = cmd.output()
        .expect("Failed to run executable");

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
    println!("Executable output: {}", stdout);

    assert!(
        stdout.contains("v20.18.1"),
        "Expected Node.js version not found in executable output: {}",
        stdout
    );
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
                    .args(&["-9", &child_id.to_string()])
                    .output();
            }
            #[cfg(windows)]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(&["/F", "/PID", &child_id.to_string()])
                    .output();
            }

            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Process timed out",
            ))
        }
    }
}
