mod common;

use anyhow::Result;
use common::{BundlerTestHelper, TestAssertions, TestProject, TestProjectManager};
use serial_test::serial;

#[tokio::test]
#[serial]
async fn test_npm_workspace_dependency_bundling() -> Result<()> {
    println!("Testing npm workspace dependency bundling...");

    // Create a workspace project with dependencies
    let project = TestProject::new("workspace-test-app")
        .workspace()
        .with_dependency("adm-zip", "^0.5.10")
        .with_dependency("commander", "^11.0.0");

    let manager = TestProjectManager::create(project)?;

    // Install dependencies in the workspace root
    manager.install_workspace_dependencies()?;

    // Verify that dependencies are installed in workspace root
    let workspace_node_modules = manager.workspace_root().unwrap().join("node_modules");
    assert!(
        workspace_node_modules.join("adm-zip").exists(),
        "adm-zip should be installed in workspace root"
    );
    assert!(
        workspace_node_modules.join("commander").exists(),
        "commander should be installed in workspace root"
    );

    // Bundle the workspace project
    let executable_path = BundlerTestHelper::bundle_project(
        manager.project_path(),
        manager.temp_dir(),
        Some("workspace-test"),
    )?;

    // Test the bundled executable
    TestAssertions::assert_executable_works(
        &executable_path,
        &[
            "Hello from workspace project!",
            "Dependencies:",
            "adm-zip",
            "commander",
            "Successfully loaded adm-zip from workspace:",
            "WORKSPACE_DEPENDENCY_TEST_PASSED",
        ],
        &[],
        &[],
    )?;

    println!("✅ npm workspace dependency bundling test passed!");
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_pnpm_workspace_dependency_bundling() -> Result<()> {
    println!("Testing pnpm workspace dependency bundling...");

    // Create a pnpm workspace project with dependencies
    let project = TestProject::new("pnpm-workspace-test-app")
        .pnpm_workspace()
        .with_dependency("adm-zip", "^0.5.10")
        .with_dependency("js-yaml", "^4.1.0");

    let manager = TestProjectManager::create(project)?;

    // Try to install dependencies using pnpm, fall back to npm if pnpm is not available
    match manager.install_pnpm_dependencies() {
        Ok(_) => {
            println!("Successfully installed pnpm workspace dependencies");
        }
        Err(e) => {
            println!("Pnpm installation failed, falling back to npm: {}", e);
            manager.install_workspace_dependencies()?;
        }
    }

    // Bundle the pnpm workspace project
    let executable_path = BundlerTestHelper::bundle_project(
        manager.project_path(),
        manager.temp_dir(),
        Some("pnpm-workspace-test"),
    )?;

    // Test the bundled executable
    TestAssertions::assert_executable_works(
        &executable_path,
        &[
            "Hello from pnpm workspace project!",
            "Dependencies:",
            "Successfully loaded adm-zip from pnpm workspace:",
            "PNPM_WORKSPACE_DEPENDENCY_TEST_PASSED",
        ],
        &[],
        &[],
    )?;

    println!("✅ pnpm workspace dependency bundling test passed!");
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_workspace_with_typescript_project() -> Result<()> {
    println!("Testing workspace with TypeScript project...");

    // Create a workspace TypeScript project
    let project = TestProject::new("workspace-ts-app")
        .workspace()
        .typescript("dist")
        .with_dependency("lodash", "^4.17.21")
        .with_dependency("@types/lodash", "^4.14.195");

    let manager = TestProjectManager::create(project)?;

    // Install dependencies
    manager.install_workspace_dependencies()?;

    // Bundle the TypeScript workspace project
    let executable_path = BundlerTestHelper::bundle_project(
        manager.project_path(),
        manager.temp_dir(),
        Some("workspace-ts-test"),
    )?;

    // Test the bundled executable
    TestAssertions::assert_executable_works(
        &executable_path,
        &[
            "Hello from TypeScript project!",
            "This should come from the compiled output directory",
            "Marker file found: dist",
        ],
        &[],
        &[],
    )?;

    println!("✅ workspace TypeScript project bundling test passed!");
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_workspace_nested_dependencies() -> Result<()> {
    println!("Testing workspace with nested dependencies...");

    // Create a workspace project that tests transitive dependency resolution
    let project = TestProject::new("nested-deps-app")
        .workspace()
        .with_dependency("express", "^4.18.2") // Has many transitive dependencies
        .with_dependency("axios", "^1.6.0"); // Also has transitive dependencies

    let manager = TestProjectManager::create(project)?;

    // Install dependencies
    manager.install_workspace_dependencies()?;

    // Create a more complex index.js that tests nested dependencies
    let complex_index_js = r#"console.log("Hello from nested dependencies test!");

try {
    const express = require('express');
    const axios = require('axios');
    
    console.log("Successfully loaded express:", typeof express);
    console.log("Successfully loaded axios:", typeof axios);
    
    // Test that transitive dependencies are available
    const app = express();
    console.log("Express app created successfully");
    
    // Test axios functionality
    console.log("Axios version:", axios.VERSION || "unknown");
    
    console.log("NESTED_DEPENDENCIES_TEST_PASSED");
} catch (e) {
    console.error("Nested dependencies test failed:", e.message);
    console.log("NESTED_DEPENDENCIES_TEST_FAILED");
    process.exit(1);
}

console.log("Nested dependencies test completed!");
process.exit(0);"#;

    std::fs::write(manager.project_path().join("index.js"), complex_index_js)?;

    // Bundle the project
    let executable_path = BundlerTestHelper::bundle_project(
        manager.project_path(),
        manager.temp_dir(),
        Some("nested-deps-test"),
    )?;

    // Test the bundled executable
    TestAssertions::assert_dependency_test_passes(
        &executable_path,
        "NESTED_DEPENDENCIES_TEST_PASSED",
    )?;

    println!("✅ workspace nested dependencies test passed!");
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_workspace_with_bin_scripts() -> Result<()> {
    println!("Testing workspace with bin scripts...");

    // Create a workspace project with dependencies that have bin scripts
    let project = TestProject::new("bin-scripts-app")
        .workspace()
        .with_dependency("semver", "^7.5.4") // Has a bin script
        .with_dependency("rimraf", "^5.0.5"); // Has a bin script

    let manager = TestProjectManager::create(project)?;

    // Install dependencies
    manager.install_workspace_dependencies()?;

    // Verify that .bin directory exists in workspace
    let workspace_bin = manager.workspace_root().unwrap().join("node_modules/.bin");
    assert!(
        workspace_bin.exists(),
        ".bin directory should exist in workspace node_modules"
    );

    // Bundle the project
    let executable_path = BundlerTestHelper::bundle_project(
        manager.project_path(),
        manager.temp_dir(),
        Some("bin-scripts-test"),
    )?;

    // Test the bundled executable
    TestAssertions::assert_executable_works(
        &executable_path,
        &[
            "Hello from workspace project!",
            "Dependencies:",
            "Workspace project test completed!",
        ],
        &[],
        &[],
    )?;

    println!("✅ workspace bin scripts test passed!");
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_workspace_project_without_own_node_modules() -> Result<()> {
    println!("Testing workspace project without its own node_modules...");

    // Create a workspace project where dependencies are only in workspace root
    let project = TestProject::new("no-local-deps-app")
        .workspace()
        .with_dependency("minimist", "^1.2.8")
        .with_dependency("chalk", "^5.3.0");

    let manager = TestProjectManager::create(project)?;

    // Install dependencies only in workspace root
    manager.install_workspace_dependencies()?;

    // Ensure the project itself has no node_modules
    let project_node_modules = manager.project_path().join("node_modules");
    if project_node_modules.exists() {
        std::fs::remove_dir_all(&project_node_modules)?;
    }

    // Verify workspace has the dependencies
    let workspace_node_modules = manager.workspace_root().unwrap().join("node_modules");
    assert!(
        workspace_node_modules.join("minimist").exists(),
        "minimist should be in workspace node_modules"
    );

    // Bundle the project
    let executable_path = BundlerTestHelper::bundle_project(
        manager.project_path(),
        manager.temp_dir(),
        Some("no-local-deps-test"),
    )?;

    // Test the bundled executable
    TestAssertions::assert_executable_works(
        &executable_path,
        &[
            "Hello from workspace project!",
            "Dependencies:",
            "Workspace project test completed!",
        ],
        &[],
        &[],
    )?;

    println!("✅ workspace project without local node_modules test passed!");
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_workspace_with_peer_dependencies() -> Result<()> {
    println!("Testing workspace with peer dependencies...");

    // Create a project that has peer dependencies
    let project = TestProject::new("peer-deps-app")
        .workspace()
        .with_dependency("react", "^18.2.0")
        .with_dependency("prop-types", "^15.8.1"); // Has react as peer dependency

    let manager = TestProjectManager::create(project)?;

    // Create a custom package.json that includes peerDependencies
    let package_json_with_peers = r#"{
  "name": "peer-deps-app",
  "version": "1.0.0",
  "main": "index.js",
  "scripts": {
    "start": "node index.js"
  },
  "dependencies": {
    "react": "^18.2.0",
    "prop-types": "^15.8.1"
  },
  "peerDependencies": {
    "react": "^18.0.0"
  }
}"#;

    std::fs::write(
        manager.project_path().join("package.json"),
        package_json_with_peers,
    )?;

    // Install dependencies
    manager.install_workspace_dependencies()?;

    // Bundle the project
    let executable_path = BundlerTestHelper::bundle_project(
        manager.project_path(),
        manager.temp_dir(),
        Some("peer-deps-test"),
    )?;

    // Test the bundled executable
    TestAssertions::assert_executable_works(
        &executable_path,
        &[
            "Hello from workspace project!",
            "Dependencies:",
            "Workspace project test completed!",
        ],
        &[],
        &[],
    )?;

    println!("✅ workspace peer dependencies test passed!");
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_deep_workspace_nesting() -> Result<()> {
    println!("Testing deep workspace nesting...");

    // Create a deeply nested workspace structure
    let project = TestProject::new("apps/frontend/client")
        .workspace()
        .with_dependency("uuid", "^9.0.1")
        .with_dependency("date-fns", "^2.30.0");

    let manager = TestProjectManager::create(project)?;

    // Install dependencies in workspace root
    manager.install_workspace_dependencies()?;

    // Bundle the deeply nested project
    let executable_path = BundlerTestHelper::bundle_project(
        manager.project_path(),
        manager.temp_dir(),
        Some("deep-nested-test"),
    )?;

    // Test the bundled executable
    TestAssertions::assert_executable_works(
        &executable_path,
        &[
            "Hello from workspace project!",
            "Dependencies:",
            "Workspace project test completed!",
        ],
        &[],
        &[],
    )?;

    println!("✅ deep workspace nesting test passed!");
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_workspace_collision_handling() -> Result<()> {
    println!("Testing workspace collision handling...");

    // Create a workspace project where the executable name might collide
    let project = TestProject::new("collision-test")
        .workspace()
        .with_dependency("fs-extra", "^11.1.1");

    let manager = TestProjectManager::create(project)?;

    // Create a directory with the same name as the expected executable
    std::fs::create_dir_all(manager.temp_dir().join("collision-test"))?;

    // Install dependencies
    manager.install_workspace_dependencies()?;

    // Bundle the project (should handle collision automatically)
    let executable_path = BundlerTestHelper::bundle_project(
        manager.project_path(),
        manager.temp_dir(),
        Some("collision-test"),
    )?;

    // The executable should exist with collision avoidance
    assert!(
        executable_path.exists(),
        "Executable should exist with collision avoidance: {}",
        executable_path.display()
    );

    // Test the bundled executable
    TestAssertions::assert_executable_works(
        &executable_path,
        &[
            "Hello from workspace project!",
            "Workspace project test completed!",
        ],
        &[],
        &[],
    )?;

    println!("✅ workspace collision handling test passed!");
    Ok(())
}
