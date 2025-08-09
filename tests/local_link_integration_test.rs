use serial_test::serial;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn write_test_local_dep_package(pkg_dir: &Path) {
    fs::create_dir_all(pkg_dir).unwrap();
    let pkg_json = r#"{
  "name": "test_local_dep",
  "version": "1.0.0",
  "main": "index.js"
}"#;
    let index_js = r#"module.exports = {
  greet() { return "test_local_dep says hi"; }
};"#;
    fs::write(pkg_dir.join("package.json"), pkg_json).unwrap();
    fs::write(pkg_dir.join("index.js"), index_js).unwrap();
}

fn write_app_with_dep(app_dir: &Path, dep_spec: &str) {
    fs::create_dir_all(app_dir).unwrap();
    let package_json = format!(
        r#"{{
  "name": "app-with-local-link",
  "version": "1.0.0",
  "main": "index.js",
  "dependencies": {{
    "test_local_dep": "{dep_spec}"
  }}
}}"#
    );
    let index_js = r#"const test_local_dep = require('test_local_dep');
console.log('Hello from app');
console.log('test_local_dep:', test_local_dep.greet());
process.exit(0);"#;
    fs::write(app_dir.join("package.json"), package_json).unwrap();
    fs::write(app_dir.join("index.js"), index_js).unwrap();
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.exists() {
        let _ = fs::remove_dir_all(dst);
    }
    fs::create_dir_all(dst)?;
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.unwrap();
        let path = entry.path();
        let rel = path.strip_prefix(src).unwrap();
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(path, &target)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    use std::os::unix::fs as unix_fs;
    if link.exists() {
        let _ = fs::remove_file(link);
        let _ = fs::remove_dir_all(link);
    }
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent)?;
    }
    unix_fs::symlink(target, link)
}

#[cfg(windows)]
fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    // Best-effort: try directory junction; fall back to copy if it fails
    if link.exists() {
        let _ = fs::remove_file(link);
        let _ = fs::remove_dir_all(link);
    }
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent)?;
    }
    std::os::windows::fs::symlink_dir(target, link).or_else(|_| copy_dir_recursive(target, link))
}

fn prepare_node_modules_for_spec(app_dir: &Path, local_pkg_dir: &Path, mode: &str) {
    let nm_test_local_dep = app_dir.join("node_modules").join("test_local_dep");
    match mode {
        "copy" => {
            copy_dir_recursive(local_pkg_dir, &nm_test_local_dep).unwrap();
        }
        "symlink" => {
            make_symlink(local_pkg_dir, &nm_test_local_dep).unwrap_or_else(|_| {
                // Fallback to copy if symlink not permitted
                copy_dir_recursive(local_pkg_dir, &nm_test_local_dep).unwrap();
            });
        }
        _ => unreachable!(),
    }
}

fn bundle_app(app_dir: &Path, out_dir: &Path, name: &str) -> PathBuf {
    // Build banderole if needed
    let bundler = {
        let target_dir = std::env::current_dir().unwrap().join("target");
        let path = if cfg!(windows) {
            target_dir.join("debug/banderole.exe")
        } else {
            target_dir.join("debug/banderole")
        };
        if !path.exists() {
            let out = Command::new("cargo").args(["build"]).output().unwrap();
            assert!(
                out.status.success(),
                "Failed to build banderole: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        path
    };

    let output = Command::new(&bundler)
        .args([
            "bundle",
            app_dir.to_str().unwrap(),
            "--no-compression",
            "--name",
            name,
        ])
        .current_dir(out_dir)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "Bundle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let exe = out_dir.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    if exe.exists() && exe.is_file() {
        exe
    } else {
        out_dir.join(if cfg!(windows) {
            format!("{name}-bundle.exe")
        } else {
            format!("{name}-bundle")
        })
    }
}

fn copy_exe_to_fresh_dir(exe: &Path) -> PathBuf {
    let run_dir = TempDir::new().unwrap();
    let dst = run_dir.path().join(exe.file_name().unwrap_or_default());
    fs::copy(exe, &dst).unwrap();
    // Keep dir alive by leaking TempDir, test process will clean temp space later
    Box::leak(Box::new(run_dir));
    dst
}

fn run_and_assert(exe: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(exe).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(exe, perms).unwrap();
    }
    let out = Command::new(exe).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "Executable failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Hello from app"));
    assert!(stdout.contains("test_local_dep: test_local_dep says hi"));
}

fn build_and_verify_portable(dep_spec: &str, mode: &str, name: &str) {
    // Each case uses its own workspace
    let root_temp = TempDir::new().unwrap();
    let root = root_temp.path().to_path_buf();

    let test_local_dep_dir = root.join("test_local_dep");
    write_test_local_dep_package(&test_local_dep_dir);

    let app = root.join("app");
    write_app_with_dep(&app, dep_spec);
    prepare_node_modules_for_spec(&app, &test_local_dep_dir, mode);

    let exe = bundle_app(&app, &root, name);
    let portable_exe = copy_exe_to_fresh_dir(&exe);

    // Remove original workspace entirely to ensure we don't accidentally resolve files from it
    let _ = fs::remove_dir_all(&root);

    run_and_assert(&portable_exe);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_local_link_variants() {
    // 1) file:../test_local_dep
    build_and_verify_portable("file:../test_local_dep", "copy", "app-file");

    // 2) link:../test_local_dep (prefer symlink; fallback to copy if not permitted)
    build_and_verify_portable("link:../test_local_dep", "symlink", "app-link");

    // 3) ../test_local_dep (bare relative)
    build_and_verify_portable("../test_local_dep", "symlink", "app-bare");
}
