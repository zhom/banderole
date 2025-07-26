mod common;

use anyhow::Result;
use common::{
    BundlerTestHelper, TestAssertions, TestCacheManager, TestProject, TestProjectManager,
};
use serial_test::serial;
use std::fs;

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_workspace_nvmrc_file_handling() -> Result<()> {
    println!("Testing workspace .nvmrc file handling...");

    // Create a workspace project
    let project = TestProject::new("nvmrc-workspace-app")
        .workspace()
        .with_dependency("lodash", "^4.17.21");

    let manager = TestProjectManager::create(project)?;

    // Create .nvmrc file in workspace root with version 20
    let workspace_root = manager.workspace_root().unwrap();
    fs::write(workspace_root.join(".nvmrc"), "20")?;

    // Install dependencies
    manager.install_workspace_dependencies()?;

    // Bundle the project
    let executable_path = BundlerTestHelper::bundle_project_with_compression(
        manager.project_path(),
        manager.temp_dir(),
        Some("nvmrc-workspace-test"),
        false,
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

    println!("✅ workspace .nvmrc file handling test passed!");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_workspace_node_version_file_handling() -> Result<()> {
    println!("Testing workspace .node-version file handling...");

    // Create a workspace project
    let project = TestProject::new("node-version-workspace-app")
        .workspace()
        .with_dependency("uuid", "^9.0.1");

    let manager = TestProjectManager::create(project)?;

    // Create .node-version file in workspace root with version 18.17.0
    let workspace_root = manager.workspace_root().unwrap();
    fs::write(workspace_root.join(".node-version"), "18.17.0")?;

    // Install dependencies
    manager.install_workspace_dependencies()?;

    // Bundle the project
    let executable_path = BundlerTestHelper::bundle_project_with_compression(
        manager.project_path(),
        manager.temp_dir(),
        Some("node-version-workspace-test"),
        false,
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

    println!("✅ workspace .node-version file handling test passed!");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_project_level_version_overrides_workspace() -> Result<()> {
    println!("Testing project-level version file overrides workspace version...");

    // Create a workspace project
    let project = TestProject::new("version-override-app")
        .workspace()
        .with_dependency("date-fns", "^2.30.0");

    let manager = TestProjectManager::create(project)?;

    // Create .nvmrc file in workspace root with version 20
    let workspace_root = manager.workspace_root().unwrap();
    fs::write(workspace_root.join(".nvmrc"), "20")?;

    // Create .nvmrc file in project directory with version 18 (should override workspace)
    fs::write(manager.project_path().join(".nvmrc"), "18")?;

    // Install dependencies
    manager.install_workspace_dependencies()?;

    // Bundle the project
    let executable_path = BundlerTestHelper::bundle_project_with_compression(
        manager.project_path(),
        manager.temp_dir(),
        Some("version-override-test"),
        false,
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

    println!("✅ project-level version override test passed!");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_version_format_compatibility() -> Result<()> {
    println!("Testing various Node version format compatibility...");

    // Test different version formats that nvmrc supports
    let test_cases = vec![
        ("23", "major-only"),
        ("23.5", "major-minor"),
        ("v20.10.0", "full-with-v-prefix"),
        ("20.10.0", "full-without-prefix"),
    ];

    for (version_spec, test_name) in test_cases {
        println!("Testing version format: {} ({})", version_spec, test_name);

        let project = TestProject::new(&format!("version-format-{}", test_name))
            .workspace()
            .with_dependency("fs-extra", "^11.1.1");

        let manager = TestProjectManager::create(project)?;

        // Create .nvmrc file with the test version
        let workspace_root = manager.workspace_root().unwrap();
        fs::write(workspace_root.join(".nvmrc"), version_spec)?;

        // Install dependencies
        manager.install_workspace_dependencies()?;

        // Bundle the project
        let executable_path = BundlerTestHelper::bundle_project_with_compression(
            manager.project_path(),
            manager.temp_dir(),
            Some(&format!("version-format-{}-test", test_name)),
            false,
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

        println!("✅ version format {} test passed!", version_spec);
    }

    println!("✅ all version format compatibility tests passed!");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_nested_workspace_package_version_resolution() -> Result<()> {
    println!("Testing nested workspace package version resolution...");

    // Create a deeply nested workspace structure
    let project = TestProject::new("nested-version-app")
        .workspace()
        .with_dependency("commander", "^11.0.0");

    let manager = TestProjectManager::create(project)?;

    // Create version files at different levels
    let workspace_root = manager.workspace_root().unwrap();
    
    // Workspace root has Node 20
    fs::write(workspace_root.join(".nvmrc"), "20")?;
    
    // Create an intermediate directory (simulating packages/ directory)
    let packages_dir = workspace_root.join("packages");
    fs::create_dir_all(&packages_dir)?;
    
    // Packages directory has Node 18 (should be ignored since project is deeper)
    fs::write(packages_dir.join(".node-version"), "18")?;

    // Install dependencies
    manager.install_workspace_dependencies()?;

    // Bundle the project
    let executable_path = BundlerTestHelper::bundle_project_with_compression(
        manager.project_path(),
        manager.temp_dir(),
        Some("nested-version-test"),
        false,
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

    println!("✅ nested workspace package version resolution test passed!");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_zzz_cleanup_workspace_version_cache() -> Result<()> {
    println!("Cleaning up application cache after workspace version tests...");

    TestCacheManager::clear_application_cache()?;

    println!("✅ Workspace version cache cleanup completed!");
    Ok(())
}
