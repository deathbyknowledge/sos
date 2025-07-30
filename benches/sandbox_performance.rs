use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use sos::http::{SoSState, create_app};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use bollard::Docker;
use tokio::sync::{Mutex, Semaphore};
use serde_json::{json, Value};
use tokio::time::Instant;

async fn benchmark_sandbox_throughput(
    num_sandboxes: usize,
    semaphore_limit: usize,
) -> anyhow::Result<(Duration, usize)> {
    // Set up test server
    let semaphore = Arc::new(Semaphore::new(semaphore_limit));
    let state = Arc::new(SoSState {
        docker: Arc::new(Docker::connect_with_local_defaults()?),
        sandboxes: Arc::new(Mutex::new(HashMap::new())),
        semaphore,
    });

    let app = create_app(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let base_url = format!("http://127.0.0.1:{}", addr.port());

    // Start server in background
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let start_time = Instant::now();

    // Create sandboxes first
    let mut sandbox_ids = Vec::new();
    for _i in 0..num_sandboxes {
        let create_payload = json!({
            "image": "ubuntu:latest",
            "setup_commands": []
        });

        if let Ok(response) = client
            .post(&format!("{}/sandboxes", base_url))
            .json(&create_payload)
            .send()
            .await
        {
            if response.status() == 200 {
                if let Ok(result) = response.json::<Value>().await {
                    if let Some(sandbox_id) = result["id"].as_str() {
                        sandbox_ids.push(sandbox_id.to_string());
                    }
                }
            }
        }
    }

    // Run all lifecycles CONCURRENTLY (this is the key difference!)
    let tasks: Vec<_> = sandbox_ids
        .into_iter()
        .enumerate()
        .map(|(i, sandbox_id)| {
            let client = client.clone();
            let base_url = base_url.clone();
            tokio::spawn(async move {
                benchmark_sandbox_lifecycle(&client, &base_url, &sandbox_id, i).await
            })
        })
        .collect();

    // Wait for all to complete
    let results = futures::future::join_all(tasks).await;
    let successful = results.iter().filter(|r| r.is_ok() && r.as_ref().unwrap().is_ok()).count();

    let total_time = start_time.elapsed();
    Ok((total_time, successful))
}

async fn benchmark_sandbox_lifecycle(
    client: &reqwest::Client,
    base_url: &str,
    sandbox_id: &str,
    task_id: usize,
) -> anyhow::Result<()> {
    // Start sandbox
    client
        .post(&format!("{}/sandboxes/{}/start", base_url, sandbox_id))
        .send()
        .await?;

    // Execute command
    let exec_payload = json!({
        "command": format!("echo 'Benchmark task {}'", task_id)
    });

    client
        .post(&format!("{}/sandboxes/{}/exec", base_url, sandbox_id))
        .json(&exec_payload)
        .send()
        .await?;

    // Cleanup
    client
        .post(&format!("{}/sandboxes/{}/stop", base_url, sandbox_id))
        .json(&json!({ "remove": true }))
        .send()
        .await?;

    Ok(())
}

fn sandbox_throughput_benchmark(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("sandbox_throughput");
    
    // Test different sandbox counts and semaphore limits
    let test_scenarios = vec![
        (5, 3),
        (10, 5),
        (20, 8),
        (30, 10),
    ];

    for (num_sandboxes, semaphore_limit) in test_scenarios {
        group.throughput(Throughput::Elements(num_sandboxes as u64));
        
        group.bench_with_input(
            BenchmarkId::new("concurrent_sandboxes", format!("{}sbox_{}sem", num_sandboxes, semaphore_limit)),
            &(num_sandboxes, semaphore_limit),
            |b, &(num_sandboxes, semaphore_limit)| {
                b.iter(|| {
                    runtime.block_on(async {
                        benchmark_sandbox_throughput(num_sandboxes, semaphore_limit)
                            .await
                            .unwrap()
                    })
                });
            },
        );
    }
    
    group.finish();
}

fn sandbox_latency_benchmark(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("single_sandbox_lifecycle", |b| {
        b.iter(|| {
            runtime.block_on(async {
                benchmark_sandbox_throughput(1, 1).await.unwrap()
            })
        });
    });
}

criterion_group!(benches, sandbox_throughput_benchmark, sandbox_latency_benchmark);
criterion_main!(benches); 