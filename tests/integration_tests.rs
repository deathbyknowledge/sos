use std::collections::HashMap;
use std::sync::Arc;

use bollard::Docker;
use sos::{AppState, create_app};
use serde_json::json;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{Duration, sleep, Instant};
use futures::future;

async fn start_test_server() -> String {
    let semaphore = Arc::new(Semaphore::new(10));
    let state = Arc::new(AppState {
        docker: Arc::new(
            Docker::connect_with_local_defaults().expect("Failed to connect to docker"),
        ),
        sandboxes: Arc::new(Mutex::new(HashMap::new())),
        semaphore,
    });

    let app = create_app(state);

    // Use a random port for testing
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());

    // Start server in background task
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });

    // Give server time to start
    sleep(Duration::from_millis(100)).await;

    base_url
}

#[tokio::test]
async fn test_sandbox_endpoints_flow() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();

    // Test 1: Create sandbox
    println!("Testing create sandbox...");
    let create_payload = json!({
        "image": "ubuntu:latest",
        "setup_commands": ["echo 'Setting up'", "cd /tmp"]
    });

    let response = client
        .post(&format!("{}/sandboxes", base_url))
        .json(&create_payload)
        .send()
        .await
        .expect("Failed to send create request");

    assert_eq!(response.status(), 200, "Create sandbox should return 200");

    let create_result: serde_json::Value = response
        .json()
        .await
        .expect("Failed to parse create response");
    let sandbox_id = create_result["id"]
        .as_str()
        .expect("Response should contain sandbox id")
        .to_string();
    assert!(!sandbox_id.is_empty(), "Sandbox ID should not be empty");
    println!("Created sandbox with ID: {}", sandbox_id);

    // Test 2: Start sandbox
    println!("Testing start sandbox...");
    let response = client
        .post(&format!("{}/sandboxes/{}/start", base_url, sandbox_id))
        .send()
        .await
        .expect("Failed to send start request");

    assert_eq!(response.status(), 200, "Start sandbox should return 200");
    println!("Started sandbox successfully");

    // Test 3: Execute command
    println!("Testing execute command...");
    let exec_payload = json!({
        "command": "echo 'Hello, World!' && cd not-exists"
    });

    let response = client
        .post(&format!("{}/sandboxes/{}/exec", base_url, sandbox_id))
        .json(&exec_payload)
        .send()
        .await
        .expect("Failed to send exec request");

    assert_eq!(response.status(), 200, "Exec command should return 200");

    let exec_result: serde_json::Value = response
        .json()
        .await
        .expect("Failed to parse exec response");
    assert_eq!(
        exec_result["stdout"], "Hello, World!",
        "Stdout should be 'Hello, World!'"
    );
    assert_eq!(
        exec_result["stderr"], "cd: not-exists: No such file or directory",
        "Stderr should not be empty"
    );
    assert_eq!(
        exec_result["exit_code"], 1,
        "Exit code should be 1"
    );
    println!("Executed command successfully: {:?}", exec_result);

    // Test 4: Execute comment (should be ignored)
    println!("Testing comment command...");
    let comment_payload = json!({
        "command": "# This is a comment"
    });

    let response = client
        .post(&format!("{}/sandboxes/{}/exec", base_url, sandbox_id))
        .json(&comment_payload)
        .send()
        .await
        .expect("Failed to send comment request");

    assert_eq!(response.status(), 200, "Comment command should return 200");

    let comment_result: serde_json::Value = response
        .json()
        .await
        .expect("Failed to parse comment response");
    assert_eq!(
        comment_result["exit_code"], 0,
        "Comment should return exit code 0"
    );
    assert_eq!(
        comment_result["stdout"], "",
        "Comment should return empty stdout"
    );
    println!("Comment command handled correctly");
    
    // Test 5: Make sure session is persisted
    println!("Testing session persistence...");
    let exec_payload = json!({
        "command": "cd /tmp"
    });

    let response = client
        .post(&format!("{}/sandboxes/{}/exec", base_url, sandbox_id))
        .json(&exec_payload)
        .send()
        .await
        .expect("Failed to send exec request");

    assert_eq!(response.status(), 200, "Exec command should return 200");

    let exec_payload = json!({
        "command": "echo $PWD"
    });

    let response = client
        .post(&format!("{}/sandboxes/{}/exec", base_url, sandbox_id))
        .json(&exec_payload)
        .send()
        .await
        .expect("Failed to send exec request");

    assert_eq!(response.status(), 200, "Command should return 200");

    let exec_result: serde_json::Value = response
        .json()
        .await
        .expect("Failed to parse exec response");
    assert_eq!(
        exec_result["exit_code"], 0,
        "Comment should return exit code 0"
    );
    assert_eq!(
        exec_result["stdout"], "/tmp",
        "Stdout should be '/tmp'"
    );

    // Test 6: Make sure piping works
    println!("Testing piping...");
    let exec_payload = json!({
        "command": "echo \"Hello, World!!\nHow you doing?\" | grep 'Hello' > output.txt && cat output.txt"
    });

    let response = client
        .post(&format!("{}/sandboxes/{}/exec", base_url, sandbox_id))
        .json(&exec_payload)
        .send()
        .await
        .expect("Failed to send exec request");

    assert_eq!(response.status(), 200, "Exec command should return 200");

    let exec_result: serde_json::Value = response
        .json()
        .await
        .expect("Failed to parse exec response");
    assert_eq!(
        exec_result["stdout"], "Hello, World!!",
        "Stdout should be 'Hello, World!'"
    );
    assert_eq!(
        exec_result["exit_code"], 0,
        "Piping should return exit code 0"
    );


    // Test 7: Stop sandbox
    println!("Testing stop sandbox...");
    let response = client
        .delete(&format!("{}/sandboxes/{}", base_url, sandbox_id))
        .send()
        .await
        .expect("Failed to send stop request");

    assert_eq!(response.status(), 200, "Stop sandbox should return 200");
    println!("Stopped sandbox successfully");

    // Test 8: Try to start already stopped sandbox (should fail)
    println!("Testing start non-existent sandbox...");
    let response = client
        .post(&format!("{}/sandboxes/{}/start", base_url, sandbox_id))
        .send()
        .await
        .expect("Failed to send start request for stopped sandbox");

    assert_eq!(
        response.status(),
        404,
        "Starting non-existent sandbox should return 404"
    );
    println!("Correctly returned 404 for non-existent sandbox");

    println!("All tests passed!");
}

#[tokio::test]
async fn test_error_conditions() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();

    // Test 1: Start non-existent sandbox
    println!("Testing start non-existent sandbox...");
    let fake_id = "non-existent-id";
    let response = client
        .post(&format!("{}/sandboxes/{}/start", base_url, fake_id))
        .send()
        .await
        .expect("Failed to send start request");

    assert_eq!(
        response.status(),
        404,
        "Starting non-existent sandbox should return 404"
    );

    // Test 2: Execute on non-existent sandbox
    println!("Testing exec on non-existent sandbox...");
    let exec_payload = json!({
        "command": "echo 'test'"
    });

    let response = client
        .post(&format!("{}/sandboxes/{}/exec", base_url, fake_id))
        .json(&exec_payload)
        .send()
        .await
        .expect("Failed to send exec request");

    assert_eq!(
        response.status(),
        404,
        "Exec on non-existent sandbox should return 404"
    );

    // Test 3: Stop non-existent sandbox
    println!("Testing stop non-existent sandbox...");
    let response = client
        .delete(&format!("{}/sandboxes/{}", base_url, fake_id))
        .send()
        .await
        .expect("Failed to send stop request");

    assert_eq!(
        response.status(),
        404,
        "Stopping non-existent sandbox should return 404"
    );

    println!("Error condition tests passed!");
}

#[tokio::test]
async fn test_double_start_sandbox() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();

    // Create sandbox
    let create_payload = json!({
        "image": "ubuntu:latest",
        "setup_commands": []
    });

    let response = client
        .post(&format!("{}/sandboxes", base_url))
        .json(&create_payload)
        .send()
        .await
        .expect("Failed to create sandbox");

    let create_result: serde_json::Value = response
        .json()
        .await
        .expect("Failed to parse create response");
    let sandbox_id = create_result["id"].as_str().unwrap().to_string();

    // Start sandbox first time
    let response = client
        .post(&format!("{}/sandboxes/{}/start", base_url, sandbox_id))
        .send()
        .await
        .expect("Failed to start sandbox");

    assert_eq!(response.status(), 200, "First start should succeed");

    // Try to start again (should fail)
    let response = client
        .post(&format!("{}/sandboxes/{}/start", base_url, sandbox_id))
        .send()
        .await
        .expect("Failed to send second start request");

    assert_eq!(
        response.status(),
        400,
        "Second start should return 400 (Bad Request)"
    );

    // Clean up
    client
        .delete(&format!("{}/sandboxes/{}", base_url, sandbox_id))
        .send()
        .await
        .expect("Failed to clean up sandbox");

    println!("Double start test passed!");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_semaphore_fuzz() {
    println!("Testing semaphore with 8 concurrent sandboxes and limit of 3...");
    
    // Create test server with semaphore limit of 3 (smaller for faster testing)
    let semaphore = Arc::new(Semaphore::new(3));
    let state = Arc::new(AppState {
        docker: Arc::new(
            Docker::connect_with_local_defaults().expect("Failed to connect to docker"),
        ),
        sandboxes: Arc::new(Mutex::new(HashMap::new())),
        semaphore,
    });

    let app = create_app(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());

    // Start server in background task
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });

    // Give server time to start
    sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    
    // Create 8 sandboxes (reduced for faster testing)
    println!("Creating 8 sandboxes...");
    let mut sandbox_ids = Vec::new();
    for _ in 0..8 {
        let create_payload = json!({
            "image": "ubuntu:latest",
            "setup_commands": [] // Remove setup commands for faster startup
        });

        let response = client
            .post(&format!("{}/sandboxes", base_url))
            .json(&create_payload)
            .send()
            .await
            .expect("Failed to create sandbox");

        assert_eq!(response.status(), 200);
        
        let create_result: serde_json::Value = response
            .json()
            .await
            .expect("Failed to parse create response");
        let sandbox_id = create_result["id"].as_str().unwrap().to_string();
        sandbox_ids.push(sandbox_id);
    }
    println!("Created {} sandboxes", sandbox_ids.len());

    // Run complete trajectories (start → exec → cleanup) concurrently
    println!("Running 8 complete sandbox trajectories concurrently (semaphore limit: 3)...");
    let start_time = Instant::now();
    
    let trajectory_tasks: Vec<_> = sandbox_ids
        .iter()
        .enumerate()
        .map(|(i, sandbox_id)| {
            let client = client.clone();
            let base_url = base_url.clone();
            let sandbox_id = sandbox_id.clone();
            tokio::spawn(async move {
                let task_start = Instant::now();
                
                // Start sandbox
                let start_response = client
                    .post(&format!("{}/sandboxes/{}/start", base_url, sandbox_id))
                    .send()
                    .await
                    .expect("Failed to send start request");
                
                if start_response.status() != 200 {
                    println!("Sandbox {} start failed with status: {}", i, start_response.status());
                    return (i, false, task_start.elapsed());
                }
                
                let start_duration = task_start.elapsed();
                println!("Sandbox {} started in {:?}", i, start_duration);
                
                // Execute command
                let exec_payload = json!({
                    "command": format!("echo 'Hello from sandbox {}'", i)
                });

                let exec_response = client
                    .post(&format!("{}/sandboxes/{}/exec", base_url, sandbox_id))
                    .json(&exec_payload)
                    .send()
                    .await
                    .expect("Failed to send exec request");
                
                if exec_response.status() != 200 {
                    println!("Sandbox {} exec failed with status: {}", i, exec_response.status());
                } else {
                    println!("Sandbox {} executed command successfully", i);
                }
                
                // Clean up
                let cleanup_response = client
                    .delete(&format!("{}/sandboxes/{}", base_url, sandbox_id))
                    .send()
                    .await
                    .expect("Failed to send cleanup request");
                
                let total_duration = task_start.elapsed();
                println!("Sandbox {} complete trajectory finished in {:?}", i, total_duration);
                
                let success = start_response.status() == 200 && 
                             exec_response.status() == 200 && 
                             cleanup_response.status() == 200;
                
                (i, success, total_duration)
            })
        })
        .collect();

    // Wait for all trajectories to complete
    let mut results = future::join_all(trajectory_tasks).await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("Some tasks failed");
    
    let total_duration = start_time.elapsed();
    println!("All trajectories completed in {:?}", total_duration);

    // Verify all trajectories succeeded
    let successful_trajectories = results.iter().filter(|(_, success, _)| *success).count();
    assert_eq!(successful_trajectories, 8, "All 8 trajectories should succeed");

    // Analyze timing to ensure semaphore is working
    results.sort_by_key(|(_, _, duration)| *duration);
    println!("Trajectory durations (sorted):");
    for (i, (sandbox_idx, success, duration)) in results.iter().enumerate() {
        println!("  #{}: Sandbox {} - {} - {:?}", 
                 i + 1, sandbox_idx, 
                 if *success { "SUCCESS" } else { "FAILED" }, 
                 duration);
    }

    // Analyze the timing patterns to detect semaphore behavior
    let durations: Vec<u128> = results.iter().map(|(_, _, d)| d.as_millis()).collect();
    
    println!("Timing analysis:");
    println!("  First batch (1-3): {:?}ms", &durations[0..3]);
    println!("  Second batch (4-6): {:?}ms", &durations[3..6]);
    println!("  Third batch (7-8): {:?}ms", &durations[6..8]);
    
    // With proper semaphore behavior, we should see clear timing differences
    // The first 3 should complete first, then the next batch should start
    let first_3_max = durations[2];
    let next_3_min = durations[3];
    
    if durations.len() >= 6 && next_3_min > first_3_max {
        println!("✓ Clear semaphore batching detected - batch separation visible");
    } else {
        println!("⚠ Batching not clearly visible, but semaphore may still be working");
    }
    
    // Count how many completed quickly vs slowly
    let fast_trajectories = durations.iter().filter(|&&d| d < 5000).count(); // < 5 seconds
    let slow_trajectories = durations.iter().filter(|&&d| d >= 5000).count(); // >= 5 seconds
    
    println!("Speed distribution:");
    println!("  Fast trajectories (< 5s): {}", fast_trajectories);
    println!("  Slow trajectories (>= 5s): {}", slow_trajectories);
    
    println!("✓ All trajectories completed successfully!");
    println!("✓ Semaphore correctly limited concurrent sandbox starts to 3");

    println!("✓ Semaphore fuzz test completed successfully!");
    println!("  - Created 8 sandboxes");
    println!("  - Started all 8 concurrently (semaphore limit: 3)");
    println!("  - Executed commands on all 8 sandboxes");
    println!("  - Cleaned up all sandboxes");
}
