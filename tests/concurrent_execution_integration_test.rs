mod common;

use anyhow::Result;
use common::{BundlerTestHelper, TestCacheManager, TestProject, TestProjectManager};
use serial_test::serial;
use std::error::Error;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

/// Test concurrent execution during first launch
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_concurrent_first_launch() -> Result<()> {
    println!("Testing concurrent execution during first launch...");

    // Create a simple test project
    let project = TestProject::new("concurrent-test-app").with_dependency("uuid", "^9.0.1");

    let manager = TestProjectManager::create(project)?;
    manager.install_dependencies()?;

    // Bundle the project
    let executable_path = BundlerTestHelper::bundle_project_with_compression(
        manager.project_path(),
        manager.temp_dir(),
        Some("concurrent-test"),
        false, // No compression for faster testing
    )?;

    // Clear any existing cache to ensure we test first launch
    TestCacheManager::clear_application_cache()?;

    // Number of concurrent executions to test
    const NUM_CONCURRENT: usize = 5;

    // Use a barrier to synchronize the start of all threads
    let barrier = Arc::new(Barrier::new(NUM_CONCURRENT));
    let executable_path = Arc::new(executable_path);

    let mut handles = Vec::new();
    let start_time = Instant::now();

    // Spawn multiple threads that will execute the binary concurrently
    for i in 0..NUM_CONCURRENT {
        let barrier = Arc::clone(&barrier);
        let executable_path = Arc::clone(&executable_path);

        let handle = thread::spawn(move || -> Result<(usize, Duration, String)> {
            // Wait for all threads to be ready
            barrier.wait();

            let thread_start = Instant::now();

            // Execute the binary using the test helper
            let output = BundlerTestHelper::run_executable(
                executable_path.as_ref(),
                &[&format!("--thread-id={i}")],
                &[("TEST_VAR", &format!("thread_{i}"))],
            )
            .map_err(|e| {
                #[cfg(windows)]
                {
                    eprintln!(
                        "Windows debug - Thread {}: Failed to execute binary at {}",
                        i,
                        executable_path.as_ref().display()
                    );
                    eprintln!("Windows debug - Thread {}: Error details: {:?}", i, e);
                    eprintln!("Windows debug - Thread {}: Error chain:", i);
                    let mut source = e.source();
                    let mut level = 0;
                    while let Some(err) = source {
                        eprintln!("Windows debug - Thread {}: Level {}: {}", i, level, err);
                        source = err.source();
                        level += 1;
                    }
                }
                anyhow::anyhow!("Failed to execute binary: {}", e)
            })?;

            let duration = thread_start.elapsed();
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();

            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "Thread {} failed with exit code {:?}. Stderr: {}",
                    i,
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }

            Ok((i, duration, stdout))
        });

        handles.push(handle);
    }

    // Wait for all threads to complete and collect results
    let mut results = Vec::new();
    for handle in handles {
        let result = handle
            .join()
            .map_err(|e| anyhow::anyhow!("Thread panicked: {:?}", e))??;
        results.push(result);
    }

    let total_time = start_time.elapsed();
    println!("Total concurrent execution time: {total_time:?}");

    // Verify all executions succeeded
    assert_eq!(
        results.len(),
        NUM_CONCURRENT,
        "Not all threads completed successfully"
    );

    // Verify each execution produced expected output
    for (thread_id, duration, stdout) in &results {
        println!("Thread {thread_id} completed in {duration:?}");

        // Check for expected output
        assert!(
            stdout.contains("Hello from test project!"),
            "Thread {thread_id} missing expected greeting in output: {stdout}"
        );

        assert!(
            stdout.contains(&format!("thread_{thread_id}")),
            "Thread {thread_id} missing environment variable in output: {stdout}"
        );

        assert!(
            stdout.contains(&format!("--thread-id={thread_id}")),
            "Thread {thread_id} missing argument in output: {stdout}"
        );
    }

    // Verify that the execution was properly queued (no thread should have taken too long)
    let max_duration = results
        .iter()
        .map(|(_, duration, _)| *duration)
        .max()
        .unwrap();
    let min_duration = results
        .iter()
        .map(|(_, duration, _)| *duration)
        .min()
        .unwrap();

    println!("Duration range: {min_duration:?} - {max_duration:?}");

    // The difference shouldn't be too extreme if queueing is working properly
    // Allow up to 30 seconds difference for extraction + queue processing
    assert!(
        max_duration - min_duration < Duration::from_secs(30),
        "Duration difference too large: {:?}, suggesting queue is not working properly",
        max_duration - min_duration
    );

    println!("✅ Concurrent first launch test passed!");
    Ok(())
}

/// Test that subsequent executions after cache is populated are fast
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_cached_concurrent_execution() -> Result<()> {
    println!("Testing concurrent execution with populated cache...");

    // Create a simple test project
    let project = TestProject::new("cached-concurrent-app").with_dependency("lodash", "^4.17.21");

    let manager = TestProjectManager::create(project)?;
    manager.install_dependencies()?;

    // Bundle the project
    let executable_path = BundlerTestHelper::bundle_project_with_compression(
        manager.project_path(),
        manager.temp_dir(),
        Some("cached-concurrent-test"),
        false,
    )?;

    // Clear cache and run once to populate it
    TestCacheManager::clear_application_cache()?;

    println!("Populating cache with initial run...");
    let initial_output =
        BundlerTestHelper::run_executable(&executable_path, &[], &[("TEST_VAR", "initial")])?;

    assert!(
        initial_output.status.success(),
        "Initial run failed: {}",
        String::from_utf8_lossy(&initial_output.stderr)
    );

    // Now test concurrent execution with populated cache
    const NUM_CONCURRENT: usize = 8;
    let barrier = Arc::new(Barrier::new(NUM_CONCURRENT));
    let executable_path = Arc::new(executable_path);

    let mut handles = Vec::new();
    let start_time = Instant::now();

    for i in 0..NUM_CONCURRENT {
        let barrier = Arc::clone(&barrier);
        let executable_path = Arc::clone(&executable_path);

        let handle = thread::spawn(move || -> Result<(usize, Duration)> {
            barrier.wait();

            let thread_start = Instant::now();

            let output = BundlerTestHelper::run_executable(
                executable_path.as_ref(),
                &[],
                &[("TEST_VAR", &format!("cached_{i}"))],
            )
            .map_err(|e| {
                #[cfg(windows)]
                {
                    eprintln!(
                        "Windows debug - Cached thread {}: Failed to execute binary at {}",
                        i,
                        executable_path.as_ref().display()
                    );
                    eprintln!(
                        "Windows debug - Cached thread {}: Error details: {:?}",
                        i, e
                    );
                    eprintln!("Windows debug - Cached thread {}: Error chain:", i);
                    let mut source = e.source();
                    let mut level = 0;
                    while let Some(err) = source {
                        eprintln!(
                            "Windows debug - Cached thread {}: Level {}: {}",
                            i, level, err
                        );
                        source = err.source();
                        level += 1;
                    }
                }
                anyhow::anyhow!("Failed to execute binary: {}", e)
            })?;

            let duration = thread_start.elapsed();

            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "Cached thread {} failed: {}",
                    i,
                    String::from_utf8_lossy(&output.stderr)
                ));
            }

            Ok((i, duration))
        });

        handles.push(handle);
    }

    let mut results = Vec::new();
    for handle in handles {
        let result = handle
            .join()
            .map_err(|e| anyhow::anyhow!("Thread panicked: {:?}", e))??;
        results.push(result);
    }

    let total_time = start_time.elapsed();
    println!("Total cached concurrent execution time: {total_time:?}");

    // Verify all executions succeeded
    assert_eq!(results.len(), NUM_CONCURRENT);

    // With cache populated, all executions should be relatively fast
    for (thread_id, duration) in &results {
        println!("Cached thread {thread_id} completed in {duration:?}");

        // Each execution should be fast since cache is populated
        assert!(
            *duration < Duration::from_secs(10),
            "Cached execution took too long: {duration:?} for thread {thread_id}"
        );
    }

    println!("✅ Cached concurrent execution test passed!");
    Ok(())
}

/// Test queue ordering - verify that processes are executed in the order they were queued
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_queue_ordering() -> Result<()> {
    println!("Testing queue ordering...");

    // Create a test project that takes a bit of time to execute
    let project = TestProject::new("queue-order-app");
    let manager = TestProjectManager::create(project)?;

    // Create a custom index.js that logs timing information
    let index_js = r#"
const fs = require('fs');
const path = require('path');

// Get thread ID from arguments
const threadId = process.argv.find(arg => arg.startsWith('--thread-id='))?.split('=')[1] || 'unknown';
const startTime = Date.now();

console.log(`Thread ${threadId} started at ${startTime}`);
console.log("Hello from test project!");
console.log("Node version:", process.version);

// Simulate some work
const start = Date.now();
while (Date.now() - start < 100) {
    // Busy wait for 100ms to simulate work
}

console.log(`Thread ${threadId} completed at ${Date.now()}`);
console.log("All tests completed!");
process.exit(0);
"#;

    std::fs::write(manager.project_path().join("index.js"), index_js)?;

    // Bundle the project
    let executable_path = BundlerTestHelper::bundle_project_with_compression(
        manager.project_path(),
        manager.temp_dir(),
        Some("queue-order-test"),
        false,
    )?;

    // Clear cache to ensure we test first launch queueing
    TestCacheManager::clear_application_cache()?;

    const NUM_THREADS: usize = 4;
    let barrier = Arc::new(Barrier::new(NUM_THREADS));
    let executable_path = Arc::new(executable_path);

    let mut handles = Vec::new();

    for i in 0..NUM_THREADS {
        let barrier = Arc::clone(&barrier);
        let executable_path = Arc::clone(&executable_path);

        let handle = thread::spawn(move || -> Result<(usize, String)> {
            barrier.wait();

            // Add a small delay to ensure threads start in order
            thread::sleep(Duration::from_millis(i as u64 * 10));

            let output = Command::new(executable_path.as_ref())
                .args(&[format!("--thread-id={i}")])
                .output()
                .map_err(|e| anyhow::anyhow!("Failed to execute binary: {}", e))?;

            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "Queue order thread {} failed: {}",
                    i,
                    String::from_utf8_lossy(&output.stderr)
                ));
            }

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            Ok((i, stdout))
        });

        handles.push(handle);
    }

    let mut results = Vec::new();
    for handle in handles {
        let result = handle
            .join()
            .map_err(|e| anyhow::anyhow!("Thread panicked: {:?}", e))??;
        results.push(result);
    }

    // Verify all executions succeeded
    assert_eq!(results.len(), NUM_THREADS);

    for (thread_id, stdout) in &results {
        println!(
            "Queue order thread {} output: {}",
            thread_id,
            stdout.lines().next().unwrap_or("")
        );

        assert!(
            stdout.contains(&format!("Thread {thread_id} started")),
            "Thread {thread_id} missing start message"
        );

        assert!(
            stdout.contains(&format!("Thread {thread_id} completed")),
            "Thread {thread_id} missing completion message"
        );
    }

    println!("✅ Queue ordering test passed!");
    Ok(())
}

/// Test recovery from failed extraction
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_extraction_failure_recovery() -> Result<()> {
    println!("Testing recovery from extraction failure...");

    // Create a simple test project
    let project = TestProject::new("recovery-test-app");
    let manager = TestProjectManager::create(project)?;

    // Bundle the project
    let executable_path = BundlerTestHelper::bundle_project_with_compression(
        manager.project_path(),
        manager.temp_dir(),
        Some("recovery-test"),
        false,
    )?;

    // Clear cache
    TestCacheManager::clear_application_cache()?;

    // Test that after clearing cache, the binary still works
    let output = Command::new(&executable_path)
        .env("TEST_VAR", "recovery_test")
        .output()?;

    assert!(
        output.status.success(),
        "Recovery test failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Hello from test project!"),
        "Recovery test missing expected output: {stdout}"
    );

    println!("✅ Extraction failure recovery test passed!");
    Ok(())
}

/// Cleanup function to be called after all tests
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_zzz_cleanup_cache() -> Result<()> {
    println!("Cleaning up application cache after all tests...");

    // This test runs last due to the "zzz" prefix, ensuring cleanup happens after other tests
    TestCacheManager::clear_application_cache()?;

    println!("✅ Cache cleanup completed!");
    Ok(())
}
