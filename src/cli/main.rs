use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bollard::Docker;
use clap::{Parser, Subcommand};
use sos::http::{CreatePayload, ExecPayload, SoSState, StopPayload};
use sos::sandbox::SandboxStatus;
use tokio::sync::{Mutex, Semaphore};
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod tui;

#[derive(Parser)]
#[command(name = "sos")]
#[command(about = "A CLI for managing sandboxed containers for shell agents")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the sandbox server
    Serve {
        /// Port to listen on
        #[arg(short, long, default_value = "3000")]
        port: u16,
        /// Maximum number of concurrent sandboxes
        #[arg(short, long, default_value = "10")]
        max_sandboxes: usize,
        /// Sandbox timeout in seconds. Default is 10 minutes.
        #[arg(long, default_value = "600")]
        timeout: u64,
    },
    /// Sandbox client commands
    Sandbox {
        /// Server URL
        #[arg(short, long, default_value = "http://localhost:3000")]
        server: String,
        #[command(subcommand)]
        action: SandboxCommands,
    },
    /// Start an interactive session with a sandbox
    Session {
        /// Server URL
        #[arg(short, long, default_value = "http://localhost:3000")]
        server: String,
        /// Container image to use
        #[arg(short, long, default_value = "ubuntu:latest")]
        image: String,
        /// Setup commands to run after container start
        #[arg(long)]
        setup: Vec<String>,
    },
    /// Start the Terminal User Interface
    Tui {
        /// Server URL
        #[arg(short, long, default_value = "http://localhost:3000")]
        server: String,
    },
}

#[derive(Subcommand)]
enum SandboxCommands {
    /// Create a new sandbox
    Create {
        /// Container image to use
        #[arg(short, long, default_value = "ubuntu:latest")]
        image: String,
        /// Setup commands to run after container start
        #[arg(short, long)]
        setup: Vec<String>,
    },
    /// List all sandboxes
    List,
    /// Start a sandbox
    Start {
        /// Sandbox ID
        id: String,
    },
    /// Execute a command in a sandbox
    Exec {
        /// Sandbox ID
        id: String,
        /// Command to execute
        command: String,
        /// Whether to execute the command in standalone mode
        #[arg(short, long, default_value = "false")]
        standalone: Option<bool>,
    },
    /// Stop and remove a sandbox
    Stop {
        /// Sandbox ID
        id: String,
        #[arg(short, long, default_value = "false")]
        remove: Option<bool>,
    },
    /// View the command trajectory of a sandbox
    Trajectory {
        /// Sandbox ID
        id: String,
        /// Whether to format output as human-readable text
        #[arg(short, long, default_value = "false")]
        formatted: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing subscriber
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // Default log level: info for your app, warn for dependencies
                "sos=info,bollard=warn,hyper=warn,tower=warn,axum=info".into()
            }),
        )
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .init();

    info!("Starting SoS (Sea of Simulation)");

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve {
            port,
            max_sandboxes,
            timeout,
        } => serve_command(port, max_sandboxes, timeout).await,
        Commands::Sandbox { server, action } => sandbox_command(server, action).await,
        Commands::Session {
            server,
            image,
            setup,
        } => session_command(server, image, setup).await,
        Commands::Tui { server } => tui_command(server).await,
    }
}

async fn serve_command(port: u16, max_sandboxes: usize, timeout: u64) -> Result<()> {
    info!(
        port = port,
        max_sandboxes = max_sandboxes,
        timeout_seconds = timeout,
        "Starting sandbox server"
    );

    // For podman, use the podman socket path
    let docker = Docker::connect_with_local_defaults()?;
    let semaphore = Arc::new(Semaphore::new(max_sandboxes));
    let state = Arc::new(SoSState {
        docker: Arc::new(docker),
        sandboxes: Arc::new(Mutex::new(HashMap::new())),
        semaphore,
    });

    let state_clone = state.clone();
    tokio::spawn(async move {
        let timeout_duration = Duration::from_secs(timeout);
        loop {
            // Check every minute
            tokio::time::sleep(Duration::from_secs(60)).await;

            let mut sandboxes_to_remove = Vec::new();
            let sandboxes = state_clone.sandboxes.lock().await;

            for (id, sandbox_arc) in sandboxes.iter() {
                let sandbox = sandbox_arc.lock().await;
                if let Some(start_time) = sandbox.start_time {
                    if start_time.elapsed() > timeout_duration {
                        warn!(sandbox_id = %id, elapsed_seconds = start_time.elapsed().as_secs(), "Sandbox timed out, removing");
                        sandboxes_to_remove.push(id.clone());
                    }
                }
            }
            drop(sandboxes); // Release the lock before removing

            for id in sandboxes_to_remove {
                // This is a simplified version of the stop_sandbox logic
                let sandbox_arc = {
                    let mut sandboxes = state_clone.sandboxes.lock().await;
                    sandboxes.remove(&id)
                };

                if let Some(sandbox_arc) = sandbox_arc {
                    let mut sandbox = sandbox_arc.lock().await;
                    if let SandboxStatus::Started(_) = sandbox.get_status() {
                        let _ = sandbox.stop().await;
                    }
                }
            }
        }
    });

    let app = sos::http::create_app(state);

    let bind_addr = format!("0.0.0.0:{}", port);
    info!(bind_address = %bind_addr, "Server listening");

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, app.into_make_service()).await?;

    Ok(())
}

async fn sandbox_command(server: String, action: SandboxCommands) -> Result<()> {
    let client = reqwest::Client::new();

    match action {
        SandboxCommands::Create { image, setup } => {
            println!("Creating sandbox with image: {}", image);
            if !setup.is_empty() {
                println!("Setup commands: {:?}", setup);
            }

            let payload = CreatePayload {
                image,
                setup_commands: setup,
            };

            let response = client
                .post(&format!("{}/sandboxes", server))
                .json(&payload)
                .send()
                .await?;

            if response.status().is_success() {
                let result: serde_json::Value = response.json().await?;
                let id = result["id"].as_str().unwrap();
                println!("✓ Sandbox created with ID: {}", id);
                println!("  Use 'sos sandbox start {}' to start it", id);
            } else {
                let error = response.text().await?;
                eprintln!("✗ Failed to create sandbox: {}", error);
                std::process::exit(1);
            }
        }
        SandboxCommands::List => {
            println!("Listing all sandboxes...");

            let response = client.get(&format!("{}/sandboxes", server)).send().await?;

            if response.status().is_success() {
                let sandboxes: Vec<serde_json::Value> = response.json().await?;

                if sandboxes.is_empty() {
                    println!("No sandboxes found");
                } else {
                    println!("{:<36} {:<20} {:<10} {}", "ID", "IMAGE", "STATUS", "SETUP");
                    println!("{}", "-".repeat(80));

                    for sandbox in sandboxes {
                        let id = sandbox["id"].as_str().unwrap_or("N/A");
                        let image = sandbox["image"].as_str().unwrap_or("N/A");
                        let status = sandbox["status"].as_str().unwrap_or("N/A");
                        let setup = sandbox["setup_commands"].as_str().unwrap_or("");
                        let setup_display = if setup.is_empty() {
                            "none".to_string()
                        } else if setup.len() > 30 {
                            format!("{}...", &setup[..27])
                        } else {
                            setup.to_string()
                        };

                        println!("{:<36} {:<20} {:<10} {}", id, image, status, setup_display);
                    }
                }
            } else {
                let error = response.text().await?;
                eprintln!("✗ Failed to list sandboxes: {}", error);
                std::process::exit(1);
            }
        }
        SandboxCommands::Start { id } => {
            println!("Starting sandbox: {}", id);

            let response = client
                .post(&format!("{}/sandboxes/{}/start", server, id))
                .send()
                .await?;

            if response.status().is_success() {
                println!("✓ Sandbox {} started successfully", id);
                println!("  Use 'sos sandbox exec {} <command>' to run commands", id);
            } else {
                let error = response.text().await?;
                eprintln!("✗ Failed to start sandbox: {}", error);
                std::process::exit(1);
            }
        }
        SandboxCommands::Exec {
            id,
            command,
            standalone,
        } => {
            println!("Executing command in sandbox {}: {}", id, command);

            let payload = ExecPayload {
                command,
                standalone,
            };

            let response = client
                .post(&format!("{}/sandboxes/{}/exec", server, id))
                .json(&payload)
                .send()
                .await?;

            if response.status().is_success() {
                let result: serde_json::Value = response.json().await?;
                let output = result["output"].as_str().unwrap_or("");
                let exit_code = result["exit_code"].as_i64().unwrap_or(-1);

                if !output.is_empty() {
                    println!("{}", output);
                }

                if exit_code != 0 {
                    eprintln!("Command failed with exit code: {}", exit_code);
                    std::process::exit(exit_code as i32);
                }
            } else {
                let error = response.text().await?;
                eprintln!("✗ Failed to execute command: {}", error);
                std::process::exit(1);
            }
        }
        SandboxCommands::Stop { id, remove } => {
            println!("Stopping sandbox: {}", id);

            let response = client
                .post(&format!("{}/sandboxes/{}/stop", server, id))
                .json(&StopPayload { remove })
                .send()
                .await?;

            if response.status().is_success() {
                println!("✓ Sandbox {} stopped", id);
                println!("  Use 'sos trajectory {}' to view command history", id);
            } else {
                let error = response.text().await?;
                eprintln!("✗ Failed to stop sandbox: {}", error);
                std::process::exit(1);
            }
        }
        SandboxCommands::Trajectory { id, formatted } => {
            println!("Viewing trajectory for sandbox: {}", id);

            if formatted {
                let response = client
                    .get(&format!("{}/sandboxes/{}/trajectory/formatted", server, id))
                    .send()
                    .await?;

                if response.status().is_success() {
                    let formatted_trajectory = response.text().await?;
                    println!("{}", formatted_trajectory);
                } else {
                    let error = response.text().await?;
                    eprintln!("✗ Failed to get trajectory: {}", error);
                    std::process::exit(1);
                }
            } else {
                let response = client
                    .get(&format!("{}/sandboxes/{}/trajectory", server, id))
                    .send()
                    .await?;

                if response.status().is_success() {
                    let trajectory_data: serde_json::Value = response.json().await?;
                    println!("{}", serde_json::to_string_pretty(&trajectory_data)?);
                } else {
                    let error = response.text().await?;
                    eprintln!("✗ Failed to get trajectory: {}", error);
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
}

async fn session_command(server: String, image: String, setup: Vec<String>) -> Result<()> {
    println!("Starting interactive session with image: {}", image);
    if !setup.is_empty() {
        println!("Setup commands: {:?}", setup);
    }

    let client = reqwest::Client::new();

    // Create the sandbox
    let payload = CreatePayload {
        image,
        setup_commands: setup,
    };

    let response = client
        .post(&format!("{}/sandboxes", server))
        .json(&payload)
        .send()
        .await?;

    let id = if response.status().is_success() {
        let result: serde_json::Value = response.json().await?;
        let id = result["id"].as_str().unwrap().to_string();
        println!("✓ Sandbox created with ID: {}", id);
        id
    } else {
        let error = response.text().await?;
        eprintln!("✗ Failed to create sandbox: {}", error);
        std::process::exit(1);
    };

    // Start the sandbox
    println!("Starting sandbox...");
    let response = client
        .post(&format!("{}/sandboxes/{}/start", server, id))
        .send()
        .await?;

    if response.status().is_success() {
        println!("✓ Sandbox started successfully");
    } else {
        let error = response.text().await?;
        eprintln!("✗ Failed to start sandbox: {}", error);
        std::process::exit(1);
    }

    // Enter interactive mode
    println!("Entering interactive session. Type 'exit' to quit.");
    println!("Session ID: {}", id);
    println!("{}", "=".repeat(50));

    loop {
        print!("\nsandbox:{}> ", &id[..8]); // Show first 8 chars of ID as prompt
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let command = input.trim();

        if command.is_empty() {
            continue;
        }

        if command.eq_ignore_ascii_case("exit") || command.eq_ignore_ascii_case("quit") {
            break;
        }

        let payload = ExecPayload {
            command: command.to_string(),
            standalone: None,
        };

        let response = client
            .post(&format!("{}/sandboxes/{}/exec", server, id))
            .json(&payload)
            .send()
            .await?;

        if response.status().is_success() {
            let result: serde_json::Value = response.json().await?;
            let output = result["output"].as_str().unwrap_or("");
            let exit_code = result["exit_code"].as_i64().unwrap_or(-1);

            if !output.is_empty() {
                print!("{}", output);
            }

            // Don't exit the session on command failure, just show exit code
            if exit_code != 0 {
                eprintln!("(exit code: {})", exit_code);
            }
        } else {
            let error = response.text().await?;
            eprintln!("✗ Failed to execute command: {}", error);
        }
    }

    // Clean up the sandbox
    println!("Stopping and removing sandbox...");
    let response = client
        .post(&format!("{}/sandboxes/{}/stop", server, id))
        .json(&StopPayload { remove: Some(true) })
        .send()
        .await?;

    if response.status().is_success() {
        println!("✓ Sandbox session ended");
    } else {
        let error = response.text().await?;
        eprintln!("⚠ Warning: Failed to clean up sandbox: {}", error);
    }

    Ok(())
}

async fn tui_command(server: String) -> Result<()> {
    tui::run_tui(server).await
}
