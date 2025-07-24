use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, post},
};
use bollard::{
    Docker,
    exec::{CreateExecOptions, StartExecOptions, StartExecResults},
    models::ContainerCreateBody,
    query_parameters::{AttachContainerOptions, RemoveContainerOptions},
};
use bytes::Bytes;
use futures::StreamExt;
use futures::channel::mpsc::{self, UnboundedReceiver};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{self, Duration, Instant};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub docker: Arc<Docker>,
    pub sandboxes: Arc<Mutex<HashMap<String, Arc<Mutex<Sandbox>>>>>,
    pub semaphore: Arc<Semaphore>,
}

#[derive(Deserialize, serde::Serialize)]
pub struct CreatePayload {
    pub image: String,
    pub setup_commands: Vec<String>,
}

#[derive(Deserialize, serde::Serialize)]
pub struct ExecPayload {
    pub command: String,
}

#[derive(Serialize)]
pub struct SandboxInfo {
    pub id: String,
    pub image: String,
    pub setup_commands: String,
    pub status: String,
}

pub struct Sandbox {
    pub id: String,
    pub image: String,
    pub setup_commands: String,
    pub container_id: Option<String>,
    pub command_id: AtomicU32,
    pub permit: Option<tokio::sync::OwnedSemaphorePermit>,
    pub input: Option<Mutex<Pin<Box<dyn tokio::io::AsyncWrite + Send>>>>,
    pub output_receiver: Option<Mutex<UnboundedReceiver<Bytes>>>,
}

impl Sandbox {
    pub async fn drain(&mut self, timeout: f64) -> Result<String> {
        let receiver = self
            .output_receiver
            .as_ref()
            .ok_or(anyhow!("No receiver"))?;
        let mut receiver = receiver.lock().await;
        let mut drained: Vec<u8> = Vec::new();
        loop {
            match time::timeout(Duration::from_secs_f64(timeout), receiver.next()).await {
                Ok(Some(b)) => drained.extend(&b),
                Ok(None) => break,
                Err(_) => break,
            }
        }
        Ok(String::from_utf8_lossy(&drained).to_string())
    }

    pub async fn read_until_marker(&mut self, marker: &str, timeout: f64) -> Result<()> {
        let receiver = self
            .output_receiver
            .as_ref()
            .ok_or(anyhow!("No receiver"))?;
        let mut receiver = receiver.lock().await;
        let mut accumulated = String::new();
        let start = Instant::now();
        while !accumulated.contains(marker) {
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed > timeout {
                return Err(anyhow!("Timeout waiting for marker: {}", marker));
            }
            let remaining = timeout - elapsed;
            match time::timeout(Duration::from_secs_f64(remaining), receiver.next()).await {
                Ok(Some(chunk)) => accumulated += &String::from_utf8_lossy(&chunk),
                Ok(None) => return Err(anyhow!("Stream closed unexpectedly")),
                Err(_) => return Err(anyhow!("Timeout waiting for marker: {}", marker)),
            }
        }
        Ok(())
    }

    pub async fn exec_standalone_cmd(
        &self,
        docker: &Docker,
        cmd: Vec<String>,
    ) -> Result<(String, String, i64)> {
        let cid = self.container_id.as_ref().ok_or(anyhow!("No container"))?;
        let exec_config = CreateExecOptions {
            cmd: Some(cmd),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            attach_stdin: Some(false),
            tty: Some(false),
            ..Default::default()
        };
        let exec = docker.create_exec(cid, exec_config).await?;
        let start_res = docker
            .start_exec(&exec.id, None::<StartExecOptions>)
            .await?;
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        if let StartExecResults::Attached { output, .. } = start_res {
            let mut output = output;
            while let Some(item) = output.next().await {
                match item? {
                    bollard::container::LogOutput::StdOut { message } => stdout.extend(&message),
                    bollard::container::LogOutput::StdErr { message } => stderr.extend(&message),
                    _ => {}
                }
            }
        }
        let inspect = docker.inspect_exec(&exec.id).await?;
        let exit_code = inspect.exit_code.unwrap_or(-1);
        let stdout_str = String::from_utf8_lossy(&stdout).to_string();
        let stderr_str = String::from_utf8_lossy(&stderr).to_string();
        Ok((stdout_str, stderr_str, exit_code))
    }
}

// Utility function for parsing command output
fn extract_section<'a>(text: &'a str, start_marker: &str, end_marker: &str) -> &'a str {
    if let Some(start_pos) = text.find(start_marker) {
        let content_start = start_pos + start_marker.len();
        if let Some(end_pos) = text[content_start..].find(end_marker) {
            let content_end = content_start + end_pos;
            return text[content_start..content_end].trim_start_matches('\n');
        }
    }
    ""
}

pub use handlers::*;

mod handlers {
    use super::*;

    pub async fn create_sandbox(
        State(state): State<Arc<AppState>>,
        Json(payload): Json<CreatePayload>,
    ) -> Result<Json<Value>, (StatusCode, String)> {
        let id = Uuid::new_v4().to_string();
        let setup = if !payload.setup_commands.is_empty() {
            payload.setup_commands.join(" && ")
        } else {
            String::new()
        };
        let sandbox = Sandbox {
            id: id.clone(),
            image: payload.image,
            setup_commands: setup,
            container_id: None,
            command_id: AtomicU32::new(0),
            permit: None,
            input: None,
            output_receiver: None,
        };
        state
            .sandboxes
            .lock()
            .await
            .insert(id.clone(), Arc::new(Mutex::new(sandbox)));
        Ok(Json(serde_json::json!({ "id": id })))
    }

    pub async fn start_sandbox(
        Path(id): Path<String>,
        State(state): State<Arc<AppState>>,
    ) -> Result<(), (StatusCode, String)> {
        let permit = state
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let sandbox_arc = {
            let sandboxes = state.sandboxes.lock().await;
            sandboxes
                .get(&id)
                .cloned()
                .ok_or((StatusCode::NOT_FOUND, "Sandbox not found".to_string()))?
        };

        // Now lock the individual sandbox and do long work
        let mut sandbox_guard = sandbox_arc.lock().await;

        if sandbox_guard.container_id.is_some() {
            return Err((
                StatusCode::BAD_REQUEST,
                "Sandbox already started".to_string(),
            ));
        }

        let config = ContainerCreateBody {
            image: Some(sandbox_guard.image.clone()),
            cmd: Some(vec!["/bin/bash".to_string(), "-i".to_string()]),
            tty: Some(false),
            open_stdin: Some(true),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };

        let create_response = state
            .docker
            .create_container(
                None::<bollard::query_parameters::CreateContainerOptions>,
                config,
            )
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        sandbox_guard.container_id = Some(create_response.id.clone());

        state
            .docker
            .start_container(
                &create_response.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        if !sandbox_guard.setup_commands.is_empty() {
            let exec_config = CreateExecOptions {
                cmd: Some(vec![
                    "/bin/bash".to_string(),
                    "-c".to_string(),
                    sandbox_guard.setup_commands.clone(),
                ]),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                attach_stdin: Some(false),
                tty: Some(false),
                ..Default::default()
            };
            let exec = state
                .docker
                .create_exec(&create_response.id, exec_config)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            let start_res = state
                .docker
                .start_exec(&exec.id, None::<bollard::exec::StartExecOptions>)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            let (mut stdout, mut stderr) = (Vec::<u8>::new(), Vec::<u8>::new());
            if let StartExecResults::Attached { mut output, .. } = start_res {
                while let Some(item) = output.next().await {
                    match item.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))? {
                        bollard::container::LogOutput::StdOut { message } => {
                            stdout.extend(&message)
                        }
                        bollard::container::LogOutput::StdErr { message } => {
                            stderr.extend(&message)
                        }
                        _ => {}
                    }
                }
            }

            let inspect = state
                .docker
                .inspect_exec(&exec.id)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            if inspect.exit_code != Some(0) {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Setup failed: {}", String::from_utf8_lossy(&stderr)),
                ));
            }
        }

        let attach_options = AttachContainerOptions {
            stdin: true,
            stdout: true,
            stderr: true,
            stream: true,
            logs: false,
            detach_keys: None,
        };

        let attach_res = state
            .docker
            .attach_container(&create_response.id, Some(attach_options))
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let mut output_stream = attach_res.output;
        let input = attach_res.input;

        let (tx, rx) = mpsc::unbounded::<Bytes>();
        tokio::spawn(async move {
            while let Some(res) = output_stream.next().await {
                if let Ok(chunk) = res {
                    let bytes = match chunk {
                        bollard::container::LogOutput::StdOut { message } => message,
                        bollard::container::LogOutput::StdErr { message } => message,
                        _ => continue,
                    };
                    let _ = tx.unbounded_send(bytes);
                } else {
                    break;
                }
            }
        });

        sandbox_guard.input = Some(Mutex::new(input));
        sandbox_guard.output_receiver = Some(Mutex::new(rx));

        {
            let mut input_guard = sandbox_guard.input.as_ref().unwrap().lock().await;
            input_guard
                .write_all("stty -echo\n".as_bytes())
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }

        sandbox_guard
            .drain(0.5)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        sandbox_guard.permit = Some(permit);

        Ok(())
    }

    pub async fn exec_session_cmd(
        Path(id): Path<String>,
        State(state): State<Arc<AppState>>,
        Json(payload): Json<ExecPayload>,
    ) -> Result<Json<Value>, (StatusCode, String)> {
        let command = payload.command;
        if command.trim_start().starts_with('#') {
            return Ok(Json(
                serde_json::json!({ "stdout": "", "stderr": "", "exit_code": 0 }),
            ));
        }

        let sandbox_arc = {
            let sandboxes = state.sandboxes.lock().await;
            sandboxes
                .get(&id)
                .cloned()
                .ok_or((StatusCode::NOT_FOUND, "Sandbox not found".to_string()))?
        };

        let mut sandbox_guard = sandbox_arc.lock().await;

        let command_id = sandbox_guard.command_id.fetch_add(1, Ordering::Relaxed);
        let stdout_file = format!("/tmp/stdout_{}.txt", command_id);
        let stderr_file = format!("/tmp/stderr_{}.txt", command_id);
        let exitcode_file = format!("/tmp/exitcode_{}.txt", command_id);
        let marker = format!("COMMAND_DONE_{}", command_id);

        let grouped_command = format!("{{ {} ; }}", command);
        let cmd_to_send = format!(
            "{} > {} 2> {}; echo $? > {}; echo '{}'\n",
            grouped_command, stdout_file, stderr_file, exitcode_file, marker
        );

        {
            let mut input = sandbox_guard
                .input
                .as_ref()
                .ok_or((StatusCode::BAD_REQUEST, "Sandbox not started".to_string()))?
                .lock()
                .await;
            input
                .write_all(cmd_to_send.as_bytes())
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }

        sandbox_guard
            .read_until_marker(&marker, 20.0)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        // Read all three files in a single exec command with delimiters
        let combined_cmd = vec![
            "/bin/bash".to_string(),
            "-c".to_string(),
            format!(
                "echo 'STDOUT_START'; cat {}; echo 'STDOUT_END'; echo 'STDERR_START'; cat {}; echo 'STDERR_END'; echo 'EXITCODE_START'; cat {}; echo 'EXITCODE_END'",
                stdout_file, stderr_file, exitcode_file
            ),
        ];

        let (combined_output, _, exec_exit_code) = sandbox_guard
            .exec_standalone_cmd(&state.docker, combined_cmd)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        if exec_exit_code != 0 {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to read output files".to_string(),
            ));
        }

        // Parse the combined output
        let stdout = extract_section(&combined_output, "STDOUT_START", "STDOUT_END")
            .trim_end_matches('\n')
            .to_string();
        let stderr = extract_section(&combined_output, "STDERR_START", "STDERR_END")
            .trim_end_matches('\n')
            .to_string();
        let exit_code_str = extract_section(&combined_output, "EXITCODE_START", "EXITCODE_END")
            .trim()
            .to_string();


        // Since we're non-tty, stderr is prefixed by `bash: `, so we need to remove that
        let stderr = stderr.trim_start_matches("bash: ").to_string();

        // Clean up files
        let clean_cmd = vec![
            "rm".to_string(),
            stdout_file.clone(),
            stderr_file.clone(),
            exitcode_file.clone(),
        ];
        let exec_config = CreateExecOptions {
            cmd: Some(clean_cmd),
            attach_stdout: Some(false),
            attach_stderr: Some(false),
            attach_stdin: Some(false),
            ..Default::default()
        };

        let cid = sandbox_guard.container_id.as_ref().unwrap();
        let exec = state
            .docker
            .create_exec(cid, exec_config)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        state
            .docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    tty: false,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        sandbox_guard
            .drain(0.5)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        Ok(Json(serde_json::json!({
            "stdout": stdout,
            "stderr": stderr,
            "exit_code": exit_code_str.parse::<i32>().unwrap_or(-1)
        })))
    }

    pub async fn stop_sandbox(
        Path(id): Path<String>,
        State(state): State<Arc<AppState>>,
    ) -> Result<(), (StatusCode, String)> {
        let sandbox_arc = {
            let mut sandboxes = state.sandboxes.lock().await;
            sandboxes
                .remove(&id)
                .ok_or((StatusCode::NOT_FOUND, format!("Sandbox {} not found", id)))?
        };

        let cid_opt = sandbox_arc.lock().await.container_id.clone();

        if let Some(cid) = cid_opt {
            let _ = state
                .docker
                .remove_container(
                    &cid,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        }

        // Sandbox_arc drops here, releasing permit if held
        Ok(())
    }

    pub async fn list_sandboxes(
        State(state): State<Arc<AppState>>,
    ) -> Result<Json<Vec<super::SandboxInfo>>, (StatusCode, String)> {
        // Brief global lock to clone all Arcs
        let sandbox_arcs = {
            let sandboxes = state.sandboxes.lock().await;
            sandboxes.values().cloned().collect::<Vec<_>>()
        };
    
        // Now process concurrently without holding global
        let futures: Vec<_> = sandbox_arcs
            .iter()
            .map(|sandbox_arc| async {
                let sandbox = sandbox_arc.lock().await;
                let status = if sandbox.container_id.is_some() {
                    "started"
                } else {
                    "created"
                };
                super::SandboxInfo {
                    id: sandbox.id.clone(),
                    image: sandbox.image.clone(),
                    setup_commands: sandbox.setup_commands.clone(),
                    status: status.to_string(),
                }
            })
            .collect();
    
        let sandbox_list = join_all(futures).await;
        Ok(Json(sandbox_list))
    }
}

pub fn create_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/sandboxes", post(create_sandbox).get(list_sandboxes))
        .route("/sandboxes/{id}/start", post(start_sandbox))
        .route("/sandboxes/{id}/exec", post(exec_session_cmd))
        .route("/sandboxes/{id}", delete(stop_sandbox))
        .with_state(state)
}
