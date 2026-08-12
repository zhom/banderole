//! Covers the launcher behaviour introduced when the runtime switched from
//! "spawn Node as a child and wait" to "exec() into Node", and when the payload
//! was reduced to just the Node executable.

mod common;

use common::{BundlerTestHelper, TestCacheManager};
use serial_test::serial;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// Writes a project whose entrypoint can exit with a chosen code or idle forever.
fn write_probe_project(root: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let app = root.join("probe-app");
    fs::create_dir_all(&app)?;

    fs::write(
        app.join("package.json"),
        r#"{
  "name": "probe-app",
  "version": "1.0.0",
  "main": "index.js"
}"#,
    )?;

    fs::write(
        app.join("index.js"),
        r#"
const mode = process.argv[2] || "hello";
if (mode === "exit") {
  process.exit(Number(process.argv[3] || 0));
} else if (mode === "idle") {
  console.log("ready");
  setInterval(() => {}, 1000);
} else if (mode === "args") {
  console.log(JSON.stringify(process.argv.slice(2)));
} else {
  console.log("probe ok");
}
"#,
    )?;

    Ok(app)
}

/// The launcher must hand the process over to Node rather than supervising it, so exit
/// codes come straight from the app and no launcher process stays resident.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_launcher_execs_into_node() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let app = write_probe_project(temp_dir.path())?;
    let out_dir = temp_dir.path().join("out");
    fs::create_dir_all(&out_dir)?;

    TestCacheManager::clear_application_cache()?;
    let exe = BundlerTestHelper::bundle_project(&app, &out_dir, Some("probe-app"))?;

    // First launch performs the extraction; second launch takes the warm path. Both must
    // behave identically.
    for pass in ["cold", "warm"] {
        let out = BundlerTestHelper::run_executable(&exe, &[], &[])?;
        assert!(
            out.status.success(),
            "{pass} launch failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("probe ok"),
            "{pass} launch produced unexpected stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    // Arbitrary non-zero exit codes must survive. Under the old spawn+wait launcher this
    // went through `status.code().unwrap_or(1)`; under exec() the kernel reports it directly.
    for code in [0, 3, 42] {
        let out = BundlerTestHelper::run_executable(&exe, &["exit", &code.to_string()], &[])?;
        assert_eq!(
            out.status.code(),
            Some(code),
            "exit code {code} was not propagated"
        );
    }

    // Arguments after the executable name must reach the script untouched.
    let out = BundlerTestHelper::run_executable(&exe, &["args", "--flag", "value"], &[])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"--flag\"") && stdout.contains("\"value\""),
        "arguments were not forwarded: {stdout}"
    );

    Ok(())
}

/// The launcher process must be *replaced* by Node, not remain as its parent: a resident
/// parent doubles the process count and the memory held while the app is idle or sleeping.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_no_launcher_process_remains_resident() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;
    use std::process::Stdio;

    let temp_dir = TempDir::new()?;
    let app = write_probe_project(temp_dir.path())?;
    let out_dir = temp_dir.path().join("out");
    fs::create_dir_all(&out_dir)?;

    TestCacheManager::clear_application_cache()?;
    let exe = BundlerTestHelper::bundle_project(&app, &out_dir, Some("probe-app"))?;

    // Warm the cache so the timing below is not racing the first-run extraction.
    BundlerTestHelper::run_executable(&exe, &[], &[])?;

    let mut child = Command::new(&exe)
        .arg("idle")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    // Wait for the app to announce itself so we know exec() has already happened.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut buf = [0u8; 32];
    let n = stdout.read(&mut buf)?;
    assert!(
        String::from_utf8_lossy(&buf[..n]).contains("ready"),
        "idle app never became ready"
    );

    // Sample /proc *before* asserting anything, then always reap the child, so a failed
    // assertion cannot leave an idle Node process behind.
    let pid = child.id();
    let children =
        fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
    let comm = fs::read_to_string(format!("/proc/{pid}/comm")).ok();

    child.kill().ok();
    child.wait().ok();

    // The spawned pid itself is now Node, so it must have no children of its own.
    assert!(
        children.trim().is_empty(),
        "launcher still has child processes ({children:?}); it did not exec into Node"
    );
    if let Some(comm) = comm {
        assert_eq!(
            comm.trim(),
            "node",
            "spawned process should have been replaced by Node"
        );
    }

    Ok(())
}

/// Only the Node executable belongs in the payload. Shipping the whole distribution added
/// ~65 MB of headers, npm and docs that the launcher can never reach.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_payload_contains_only_node_executable() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let app = write_probe_project(temp_dir.path())?;
    let out_dir = temp_dir.path().join("out");
    fs::create_dir_all(&out_dir)?;

    TestCacheManager::clear_application_cache()?;
    let exe = BundlerTestHelper::bundle_project(&app, &out_dir, Some("probe-app"))?;

    // A full Node distribution is ~186 MB uncompressed; the compressed single binary is
    // well under a third of that. The bound is deliberately loose so ordinary Node version
    // drift does not make this flaky.
    let size = fs::metadata(&exe)?.len();
    assert!(
        size < 80 * 1024 * 1024,
        "bundle is {size} bytes; expected well under 80 MB, so the payload is likely \
         carrying the whole Node distribution again"
    );

    // Run it so the cache directory is populated, then inspect what actually landed there.
    let out = BundlerTestHelper::run_executable(&exe, &[], &[])?;
    assert!(out.status.success());

    let cache_root = TestCacheManager::application_cache_dir();
    let app_dir = fs::read_dir(&cache_root)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.is_dir() && p.join("node").exists())
        .ok_or("no extracted bundle found in the cache directory")?;

    let node_dir = app_dir.join("node");
    let node_binary = if cfg!(windows) {
        node_dir.join("node.exe")
    } else {
        node_dir.join("bin").join("node")
    };
    assert!(
        node_binary.exists(),
        "extracted bundle is missing the Node executable at {}",
        node_binary.display()
    );

    for unwanted in ["include", "share"] {
        assert!(
            !node_dir.join(unwanted).exists(),
            "extracted bundle still ships node/{unwanted}"
        );
    }
    assert!(
        !node_dir.join("lib").join("node_modules").exists(),
        "extracted bundle still ships npm/corepack under node/lib/node_modules"
    );

    Ok(())
}
