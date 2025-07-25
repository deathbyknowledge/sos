use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, post},
};
use bollard::Docker;
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

use crate::sandbox::*;

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
    pub standalone: Option<bool>,
}

#[derive(Serialize)]
pub struct SandboxInfo {
    pub id: String,
    pub image: String,
    pub setup_commands: String,
    pub status: String,
}

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
    let sandbox = Sandbox::new(id.clone(), payload.image, setup, state.docker.clone());
    state
        .sandboxes
        .lock()
        .await
        .insert(id.clone(), Arc::new(Mutex::new(sandbox)));
    Ok(Json(serde_json::json!({ "id": id })))
}

// TODO: we could read from /etc/motd to get a first message after the task.
// Could include instructions on custom tools or w/e
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

    // TODO: accurate errors
    sandbox_guard
        .start(permit)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok(())
}

pub async fn exec_cmd(
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
    let standalone = payload.standalone.unwrap_or(false);

    let (stdout, stderr, exit_code) = match standalone {
        true => sandbox_guard
            .exec_standalone_cmd(vec![command])
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        false => sandbox_guard
            .exec_session_cmd(command)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    };

    Ok(Json(serde_json::json!({
        "stdout": stdout,
        "stderr": stderr,
        "exit_code": exit_code
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

    sandbox_arc.lock().await.stop().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to stop sandbox {}: {}", id, e),
        )
    })?;

    // Sandbox_arc drops here, releasing permit if held
    Ok(())
}

pub async fn list_sandboxes(
    State(state): State<Arc<AppState>>,
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
            let status = if sandbox.container_id.is_some() {
                "started"
            } else {
                "created"
            };
            SandboxInfo {
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

pub fn create_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/sandboxes", post(create_sandbox).get(list_sandboxes))
        .route("/sandboxes/{id}/start", post(start_sandbox))
        .route("/sandboxes/{id}/exec", post(exec_cmd))
        .route("/sandboxes/{id}", delete(stop_sandbox))
        .with_state(state)
}
