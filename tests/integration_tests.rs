use std::collections::HashMap;
use std::sync::Arc;

use bollard::Docker;
use serde_json::json;
use sos::http::{SoSState, create_app};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{Duration, sleep};

// Helpers
async fn start_test_server() -> String {
    let semaphore = Arc::new(Semaphore::new(10));
    let state = Arc::new(SoSState {
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

async fn create_and_start_sandbox(client: &reqwest::Client, base_url: &str) -> String {
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

    let create_result: serde_json::Value = response.json().await.unwrap();
    let sandbox_id = create_result["id"].as_str().unwrap().to_string();

    client
        .post(&format!("{}/sandboxes/{}/start", base_url, sandbox_id))
        .send()
        .await
        .expect("Failed to start sandbox");

    sandbox_id
}

async fn cleanup_sandbox(client: &reqwest::Client, base_url: &str, sandbox_id: &str) {
    client
        .post(&format!("{}/sandboxes/{}/stop", base_url, sandbox_id))
        .json(&json!({ "remove": true }))
        .send()
        .await
        .expect("Failed to cleanup sandbox");
}

async fn execute_command(
    client: &reqwest::Client,
    base_url: &str,
    sandbox_id: &str,
    command: &str,
    standalone: Option<bool>,
) -> serde_json::Value {
    let mut exec_payload = json!({
        "command": command
    });

    if let Some(standalone_value) = standalone {
        exec_payload["standalone"] = json!(standalone_value);
    }

    let response = client
        .post(&format!("{}/sandboxes/{}/exec", base_url, sandbox_id))
        .json(&exec_payload)
        .send()
        .await
        .expect("Failed to send exec request");

    let status = response.status().clone();
    assert_eq!(
        status,
        200,
        "{}",
        response.text().await.unwrap()
    );

    response
        .json()
        .await
        .expect("Failed to parse exec response")
}

#[tokio::test]
async fn test_create_sandbox() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();

    let create_payload = json!({
        "image": "ubuntu:latest",
        "setup_commands": ["cd /tmp", "echo 'Setting up' > /tmp/setup.txt"]
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
        .expect("Response should contain sandbox id");
    assert!(!sandbox_id.is_empty(), "Sandbox ID should not be empty");
}

#[tokio::test]
async fn test_start_sandbox() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();

    // Create sandbox first
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

    let create_result: serde_json::Value = response.json().await.unwrap();
    let sandbox_id = create_result["id"].as_str().unwrap();

    // Test starting the sandbox
    let response = client
        .post(&format!("{}/sandboxes/{}/start", base_url, sandbox_id))
        .send()
        .await
        .expect("Failed to send start request");

    assert_eq!(response.status(), 200, "Start sandbox should return 200");

    // Cleanup
    client
        .post(&format!("{}/sandboxes/{}/stop", base_url, sandbox_id))
        .json(&json!({ "remove": true }))
        .send()
        .await
        .expect("Failed to cleanup sandbox");
}

#[tokio::test]
async fn test_execute_command() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();

    // Setup sandbox
    let sandbox_id = create_and_start_sandbox(&client, &base_url).await;

    // Test executing a command
    let exec_result = execute_command(
        &client,
        &base_url,
        &sandbox_id,
        "echo 'Hello, World!' && cd not-exists",
        None,
    )
    .await;

    assert_eq!(
        exec_result["output"], "Hello, World!\nbash: cd: not-exists: No such file or directory",
        "Output should contain stdout and stderr"
    );
    assert_eq!(exec_result["exit_code"], 1, "Exit code should be 1");

    // Cleanup
    cleanup_sandbox(&client, &base_url, &sandbox_id).await;
}

#[tokio::test]
async fn test_comment_commands() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();

    // Setup sandbox
    let sandbox_id = create_and_start_sandbox(&client, &base_url).await;

    // Test executing a comment (should be ignored)
    let comment_result =
        execute_command(&client, &base_url, &sandbox_id, "# This is a comment", None).await;

    assert_eq!(
        comment_result["exit_code"], 0,
        "Comment should return exit code 0"
    );
    assert_eq!(
        comment_result["output"], "",
        "Comment should return empty output"
    );

    // Cleanup
    cleanup_sandbox(&client, &base_url, &sandbox_id).await;
}

#[tokio::test]
async fn test_session_persistence() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();

    // Create sandbox with setup commands
    let create_payload = json!({
        "image": "ubuntu:latest",
        "setup_commands": ["cd /tmp", "echo 'Setting up' > /tmp/setup.txt"]
    });

    let response = client
        .post(&format!("{}/sandboxes", base_url))
        .json(&create_payload)
        .send()
        .await
        .expect("Failed to create sandbox");

    let create_result: serde_json::Value = response.json().await.unwrap();
    let sandbox_id = create_result["id"].as_str().unwrap();

    // Start sandbox
    client
        .post(&format!("{}/sandboxes/{}/start", base_url, sandbox_id))
        .send()
        .await
        .expect("Failed to start sandbox");

    // Change directory in one command
    execute_command(&client, &base_url, &sandbox_id, "cd /tmp", None).await;

    // Check if we can access the file created during setup in the current directory
    let exec_result = execute_command(&client, &base_url, &sandbox_id, "cat setup.txt", None).await;

    assert_eq!(exec_result["exit_code"], 0, "Should return exit code 0");
    assert_eq!(
        exec_result["output"], "Setting up",
        "Should read the setup file"
    );

    // Cleanup
    cleanup_sandbox(&client, &base_url, sandbox_id).await;
}

#[tokio::test]
async fn test_standalone_mode() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();

    // Setup sandbox
    let sandbox_id = create_and_start_sandbox(&client, &base_url).await;

    // First change directory
    execute_command(&client, &base_url, &sandbox_id, "cd /tmp", None).await;

    // Test standalone mode (should not persist session)
    let exec_result = execute_command(&client, &base_url, &sandbox_id, "pwd", Some(true)).await;

    assert_eq!(exec_result["exit_code"], 0, "Should return exit code 0");
    assert_eq!(
        exec_result["output"], "/\n",
        "Should be in root directory, not /tmp"
    );

    // Cleanup
    cleanup_sandbox(&client, &base_url, &sandbox_id).await;
}

#[tokio::test]
async fn test_piping_and_redirection() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();

    // Setup sandbox
    let sandbox_id = create_and_start_sandbox(&client, &base_url).await;

    // Test piping and redirection
    let exec_result = execute_command(&client, &base_url, &sandbox_id, "echo -e 'Hello, World!!\nHow you doing?\nHello' | grep 'Hello' > output.txt && cat output.txt", None).await;

    assert_eq!(
        exec_result["output"], "Hello, World!!\nHello",
        "Output should contain 'Hello, World!!' and 'Hello'"
    );
    assert_eq!(
        exec_result["exit_code"], 0,
        "Piping should return exit code 0"
    );

    let exec_result = execute_command(&client, &base_url, &sandbox_id, "echo 'Q0xVIFdBUyBIRVJF' | base64 -d ", None).await;
    assert_eq!(
        exec_result["output"], "CLU WAS HERE",
        "Output should contain 'CLU WAS HERE'"
    );
    assert_eq!(
        exec_result["exit_code"], 0,
        "Piping should return exit code 0"
    );

    // Cleanup
    cleanup_sandbox(&client, &base_url, &sandbox_id).await;
}

#[tokio::test]
async fn test_stop_sandbox() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();

    // Setup sandbox
    let sandbox_id = create_and_start_sandbox(&client, &base_url).await;

    // Test stopping the sandbox
    let response = client
        .post(&format!("{}/sandboxes/{}/stop", base_url, sandbox_id))
        .json(&json!({ "remove": true }))
        .send()
        .await
        .expect("Failed to send stop request");

    assert_eq!(response.status(), 200, "Stop sandbox should return 200");

    // Test that sandbox is actually stopped by trying to start it again
    let response = client
        .post(&format!("{}/sandboxes/{}/start", base_url, sandbox_id))
        .send()
        .await
        .expect("Failed to send start request for stopped sandbox");

    assert_eq!(
        response.status(),
        404,
        "Starting removed sandbox should return 404"
    );
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

    // Test 2: Execute on non-existent sandbox (should return 404, not 200)
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
        .post(&format!("{}/sandboxes/{}/stop", base_url, fake_id))
        .json(&json!({ "remove": true }))
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
    let sandbox_id = create_and_start_sandbox(&client, &base_url).await;

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
        .post(&format!("{}/sandboxes/{}/stop", base_url, sandbox_id))
        .json(&json!({ "remove": true }))
        .send()
        .await
        .expect("Failed to clean up sandbox");

    println!("Double start test passed!");
}
#[tokio::test]
async fn test_exit_code_preservation() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();
    let sandbox_id = create_and_start_sandbox(&client, &base_url).await;

    // Run a failing command
    let exec_result = execute_command(&client, &base_url, &sandbox_id, "false", None).await;
    assert_eq!(exec_result["exit_code"], 1, "Exit code should be 1");

    // Check $? in session
    let check_result = execute_command(&client, &base_url, &sandbox_id, "echo $?", None).await;
    assert_eq!(check_result["output"], "1", "Should see previous exit code");
    assert_eq!(check_result["exit_code"], 0, "echo $? should succeed");

    cleanup_sandbox(&client, &base_url, &sandbox_id).await;
}

#[tokio::test]
async fn test_multiline_commands() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();
    let sandbox_id = create_and_start_sandbox(&client, &base_url).await;

    // Test multi-line command with literal newlines
    let multiline_command = "echo 'First line'\necho 'Second line'\necho 'Third line'";
    let exec_result =
        execute_command(&client, &base_url, &sandbox_id, multiline_command, None).await;

    assert_eq!(
        exec_result["exit_code"], 0,
        "Multi-line command should succeed"
    );
    assert_eq!(
        exec_result["output"], "First line\nSecond line\nThird line",
        "Output should contain all three lines"
    );

    // Test multi-line command with variable assignment and usage
    let script_command = "NAME='World'\necho \"Hello, $NAME!\"\necho \"Goodbye, $NAME!\"";
    let script_result =
        execute_command(&client, &base_url, &sandbox_id, script_command, None).await;

    assert_eq!(script_result["exit_code"], 0, "Script should succeed");
    assert_eq!(
        script_result["output"], "Hello, World!\nGoodbye, World!",
        "Script output should show variable substitution"
    );

    // Test multi-line command with conditional logic
    let conditional_command =
        "if [ 1 -eq 1 ]; then\n  echo 'Condition is true'\nelse\n  echo 'Condition is false'\nfi";
    let conditional_result =
        execute_command(&client, &base_url, &sandbox_id, conditional_command, None).await;

    assert_eq!(
        conditional_result["exit_code"], 0,
        "Conditional should succeed"
    );
    assert_eq!(
        conditional_result["output"], "Condition is true",
        "Conditional output should show correct branch"
    );

    // Test multi-line command with loop
    let loop_command = "for i in 1 2 3; do\n  echo \"Number: $i\"\ndone";
    let loop_result = execute_command(&client, &base_url, &sandbox_id, loop_command, None).await;

    assert_eq!(loop_result["exit_code"], 0, "Loop should succeed");
    assert_eq!(
        loop_result["output"], "Number: 1\nNumber: 2\nNumber: 3",
        "Loop output should show all iterations"
    );

    cleanup_sandbox(&client, &base_url, &sandbox_id).await;
}

#[tokio::test]
async fn test_exit_command_response_includes_exit_true() {
    let base_url = start_test_server().await;
    let client = reqwest::Client::new();
    let sandbox_id = create_and_start_sandbox(&client, &base_url).await;

    let exec_result = execute_command(&client, &base_url, &sandbox_id, "echo hi; exit 7; echo bye", None).await;
    assert_eq!(
        exec_result.get("exited"),
        Some(&serde_json::Value::Bool(true)),
        "Response should include 'exit': true for 'echo hi; exit 7; echo bye'"
    );
    assert_eq!(
        exec_result["output"], "hi",
        "Output should include only 'hi'"
    );

    let exec_result = execute_command(&client, &base_url, &sandbox_id, "echo 'Container still running'", Some(true)).await;
    assert_eq!(
        exec_result["output"], "Container still running\n",
        "Output should include 'Container still running'"
    );
    assert_eq!(
        exec_result["exit_code"], 0,
        "Exit code should be 0"
    );
    assert_eq!(
        exec_result.get("exited"),
        Some(&serde_json::Value::Bool(false)),
        "Response should include 'exit': false for 'echo 'Container still running'"
    );

    cleanup_sandbox(&client, &base_url, &sandbox_id).await;
}
