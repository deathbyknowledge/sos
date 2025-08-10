use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::post,
};
use bollard::Docker;
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    sync::{Mutex, Semaphore},
    time::Instant,
};

use crate::sandbox::*;

impl From<SandboxError> for (StatusCode, String) {
    fn from(err: SandboxError) -> Self {
        (err.to_status_code(), err.to_string())
    }
}

impl SandboxError {
    fn to_status_code(&self) -> StatusCode {
        match self {
            SandboxError::NotStarted => StatusCode::BAD_REQUEST,
            SandboxError::AlreadyStarted => StatusCode::BAD_REQUEST,
            SandboxError::AlreadyExited => StatusCode::BAD_REQUEST,
            SandboxError::SetupCommandsFailed(_) => StatusCode::BAD_REQUEST,
            SandboxError::PullImageFailed { .. } => StatusCode::BAD_REQUEST,
            SandboxError::StopContainerFailed(_) => StatusCode::BAD_REQUEST,
            SandboxError::StartContainerFailed { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            SandboxError::ContainerWriteFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
            SandboxError::ContainerReadFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
            SandboxError::ExecFailed(_, _) => StatusCode::INTERNAL_SERVER_ERROR,
            SandboxError::CreateExecFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
            SandboxError::TimeoutWaitingForMarker(_) => StatusCode::GATEWAY_TIMEOUT,
        }
    }
}

/// Shared state for the SoS server.
/// Includes the docker client, the sandboxes map, and the semaphore.
#[derive(Clone)]
pub struct SoSState {
    pub docker: Arc<Docker>,
    pub sandboxes: Arc<Mutex<HashMap<String, Arc<Mutex<Sandbox>>>>>,
    pub semaphore: Arc<Semaphore>,
}

/// POST `/sandboxes` payload.
///
/// Includes the container image to use and the setup commands to run
/// on container startup. Setup commands will be chained together with `&&`.
#[derive(Deserialize, serde::Serialize)]
pub struct CreatePayload {
    pub image: String,
    pub setup_commands: Vec<String>,
}

/// POST `/sandboxes` handler.
///
/// Creates a new sandbox with the provided image and setup commands.
/// Assigns a new UUID to the sandbox, and returns it. Does NOT start a container.
pub async fn create_sandbox(
    State(state): State<Arc<SoSState>>,
    Json(payload): Json<CreatePayload>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let setup = if !payload.setup_commands.is_empty() {
        payload.setup_commands.join(" && ")
    } else {
        String::new()
    };
    let sandbox = Sandbox::new(payload.image, setup, state.docker.clone());
    let id = sandbox.id.clone();
    state
        .sandboxes
        .lock()
        .await
        .insert(id.clone(), Arc::new(Mutex::new(sandbox)));
    Ok(Json(serde_json::json!({ "id": id })))
}

// TODO: we could read from /etc/motd to get a first message after the task.
// Could include instructions on custom tools or w/e

/// POST `/sandboxes/{id}/start` handler.
///
/// Starts a sandbox with the given ID and runs the setup commands.
/// Acquires a permit from the semaphore, and locks the sandbox until the
/// sandbox is stopped. If no permits are available, it blocks until one is.
pub async fn start_sandbox(
    Path(id): Path<String>,
    State(state): State<Arc<SoSState>>,
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

    sandbox_guard.start(permit).await?;

    Ok(())
}

/// POST `/sandboxes/{id}/exec` payload.
///
/// Includes the command to execute and whether it should be run in standalone
/// mode.
#[derive(Deserialize, serde::Serialize)]
pub struct ExecPayload {
    pub command: String,
    pub standalone: Option<bool>,
}

/// POST `/sandboxes/{id}/exec` handler.
///
/// Executes a command in the sandbox.
/// If the command is run in standalone mode, it will be run as a new process.
/// Otherwise, it will be run in the existing session.
/// Returns the stdout, stderr, and exit code of the command.
pub async fn exec_cmd(
    Path(id): Path<String>,
    State(state): State<Arc<SoSState>>,
    Json(payload): Json<ExecPayload>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let command = payload.command;

    let sandbox_arc = {
        let sandboxes = state.sandboxes.lock().await;
        sandboxes
            .get(&id)
            .cloned()
            .ok_or((StatusCode::NOT_FOUND, "Sandbox not found".to_string()))?
    };

    let mut sandbox_guard = sandbox_arc.lock().await;
    let standalone = payload.standalone.unwrap_or(false);

    let CommandResult { output, exit_code, exited } = match standalone {
        true => sandbox_guard.exec_standalone_cmd(command).await?,
        false => sandbox_guard.exec_session_cmd(command).await?,
    };

    Ok(Json(serde_json::json!({
        "output": output,
        "exit_code": exit_code,
        "exited": exited
    })))
}

/// POST `/sandboxes/{id}/stop` payload.
///
/// Includes a flag for whether to remove the sandbox after stopping it.
#[derive(Deserialize, serde::Serialize)]
pub struct StopPayload {
    pub remove: Option<bool>,
}

/// POST `/sandboxes/{id}/stop` handler.
///
/// Stops a sandbox with the given ID.
/// If the `remove` flag is set, the sandbox will be removed from the server.
/// Otherwise, the sandbox will be stopped and remain in the server.
pub async fn stop_sandbox(
    Path(id): Path<String>,
    State(state): State<Arc<SoSState>>,
    Json(payload): Json<StopPayload>,
) -> Result<(), (StatusCode, String)> {
    let sandbox_arc = {
        let remove = payload.remove.unwrap_or(false);
        let mut sandboxes = state.sandboxes.lock().await;
        let opt = match remove {
            true => sandboxes.remove(&id),
            false => sandboxes.get(&id).cloned(),
        };
        opt.ok_or((StatusCode::NOT_FOUND, format!("Sandbox {} not found", id)))?
    };

    // Permit is released here
    sandbox_arc.lock().await.stop().await?;

    Ok(())
}

/// GET `/sandboxes/{id}/trajectory` handler.
///
/// Returns the trajectory of the sandbox.
/// The trajectory is a list of commands that have been executed in the sandbox.
/// Each command has a timestamp, a command string, and a result.
/// The result is the stdout, stderr, and exit code of the command.
pub async fn get_trajectory(
    Path(id): Path<String>,
    State(state): State<Arc<SoSState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let sandbox_arc = {
        let sandboxes = state.sandboxes.lock().await;
        sandboxes
            .get(&id)
            .cloned()
            .ok_or((StatusCode::NOT_FOUND, format!("Sandbox {} not found", id)))?
    };

    let sandbox = sandbox_arc.lock().await;
    let trajectory = sandbox.get_trajectory();

    let start_time = sandbox.start_time.unwrap_or(Instant::now());
    let trajectory_json: Vec<Value> = trajectory
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let timestamp = (cmd.timestamp - start_time).as_secs_f64();
            let mut cmd_json = serde_json::json!({
                "index": i,
                "command": cmd.command,
                "timestamp": timestamp,
            });

            if let Some(result) = &cmd.result {
                cmd_json["result"] = serde_json::json!({
                    "output": result.output,
                    "exit_code": result.exit_code,
                });
            }

            cmd_json
        })
        .collect();

    Ok(Json(serde_json::json!({
        "sandbox_id": id,
        "command_count": sandbox.command_count(),
        "trajectory": trajectory_json
    })))
}

/// GET `/sandboxes/{id}/trajectory/formatted` handler.
///
/// Returns the trajectory of the sandbox in a formatted string.
/// The trajectory is a list of commands that have been executed in the sandbox.
/// Each command has a timestamp, a command string, and a result.
pub async fn get_trajectory_formatted(
    Path(id): Path<String>,
    State(state): State<Arc<SoSState>>,
) -> Result<String, (StatusCode, String)> {
    let sandbox_arc = {
        let sandboxes = state.sandboxes.lock().await;
        sandboxes
            .get(&id)
            .cloned()
            .ok_or((StatusCode::NOT_FOUND, format!("Sandbox {} not found", id)))?
    };

    let sandbox = sandbox_arc.lock().await;
    Ok(sandbox.format_trajectory())
}

/// GET `/sandboxes` response struct.
///
/// Includes the ID, image, setup commands, and status of the sandbox.
#[derive(Serialize, Deserialize)]
pub struct SandboxInfo {
    pub id: String,
    pub image: String,
    pub setup_commands: String,
    pub status: String,
    pub session_command_count: usize,
    pub last_standalone_exit_code: Option<i64>,
}

/// GET `/sandboxes` handler.
///
/// Returns a list of all sandboxes.
/// Each sandbox has an ID, image, setup commands, and status.
pub async fn list_sandboxes(
    State(state): State<Arc<SoSState>>,
) -> Result<Json<Vec<SandboxInfo>>, (StatusCode, String)> {
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
            let status = sandbox.get_status();
            SandboxInfo {
                id: sandbox.id.clone(),
                image: sandbox.image.clone(),
                setup_commands: sandbox.setup_commands.clone(),
                status: status.to_string(),
                session_command_count: sandbox.command_count(),
                last_standalone_exit_code: sandbox.get_last_standalone_exit_code(),
            }
        })
        .collect();

    let sandbox_list = join_all(futures).await;
    Ok(Json(sandbox_list))
}

/// Creates a new router for the SoS server.
pub fn create_app(state: Arc<SoSState>) -> Router {
    Router::new()
        .route("/sandboxes", post(create_sandbox).get(list_sandboxes))
        .route("/sandboxes/{id}/start", post(start_sandbox))
        .route("/sandboxes/{id}/exec", post(exec_cmd))
        .route(
            "/sandboxes/{id}/trajectory",
            axum::routing::get(get_trajectory),
        )
        .route(
            "/sandboxes/{id}/trajectory/formatted",
            axum::routing::get(get_trajectory_formatted),
        )
        .route("/sandboxes/{id}/stop", post(stop_sandbox))
        .with_state(state)
}
