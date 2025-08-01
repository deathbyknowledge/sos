use std::{pin::Pin, sync::Arc};

use bollard::{
    Docker,
    container::LogOutput,
    exec::{CreateExecOptions, StartExecOptions, StartExecResults},
    query_parameters::RemoveContainerOptions,
};
use bytes::Bytes;
use futures::{StreamExt, channel::mpsc::UnboundedReceiver};
use strip_ansi_escapes::strip_str;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::time::{self, Duration, Instant};
use tokio::{io::AsyncWriteExt, sync::OwnedSemaphorePermit};
use tracing::error;
use uuid::Uuid;

type Result<T> = std::result::Result<T, SandboxError>;

#[derive(Error, Debug, Clone)]
pub enum SandboxError {
    #[error("Sandbox not started")]
    NotStarted,
    #[error("Sandbox already started")]
    AlreadyStarted,
    #[error("Setup commands failed: {0}")]
    SetupCommandsFailed(String),
    #[error("Failed to pull image: {0}")]
    PullImageFailed(String),
    #[error("Failed to stop container: {0}")]
    StopContainerFailed(String),
    #[error("Failed to start container {0}")]
    StartContainerFailed(String),
    #[error("Container write failed: {0}")]
    ContainerWriteFailed(String),
    #[error("Container read failed: {0}")]
    ContainerReadFailed(String),
    #[error("Exec failed: {0} (exit code: {1})")]
    ExecFailed(String, i64),
    #[error("Failed to create exec: {0}")]
    CreateExecFailed(String),
    #[error("Timeout waiting for marker: {0}")]
    TimeoutWaitingForMarker(String),
}

#[derive(Debug, Clone)]
pub enum SandboxStatus {
    Created,
    Started(String),     // container id
    Stopped(Result<()>), // result of stop
}

impl std::fmt::Display for SandboxStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxStatus::Created => write!(f, "created"),
            SandboxStatus::Started(_) => write!(f, "started"),
            SandboxStatus::Stopped(_) => write!(f, "stopped"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommandExecution {
    pub command: String,
    pub timestamp: Instant,
    pub result: Option<CommandResult>,
}

#[derive(Debug, Clone)]
pub struct CommandResult {
    pub output: String,
    pub exit_code: i64,
}

pub struct Sandbox {
    pub id: String,
    pub image: String,
    pub setup_commands: String,
    pub start_time: Option<Instant>,
    status: SandboxStatus,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    input: Option<Mutex<Pin<Box<dyn tokio::io::AsyncWrite + Send>>>>,
    output_receiver: Option<Mutex<UnboundedReceiver<Bytes>>>,
    docker: Arc<Docker>,
    trajectory: Vec<CommandExecution>,
    last_standalone_exit_code: Option<i64>,
}

impl Sandbox {
    pub fn new(image: String, setup_commands: String, docker: Arc<Docker>) -> Self {
        Sandbox {
            id: Uuid::new_v4().to_string(),
            image,
            setup_commands,
            docker,
            status: SandboxStatus::Created,
            permit: None,
            input: None,
            output_receiver: None,
            start_time: None,
            trajectory: Vec::new(),
            last_standalone_exit_code: None,
        }
    }

    // We use PS1 to track the end of command outputs AND their exit codes.
    // To make jailbreaking harder, we use the sandbox UUID as the PS1 marker.
    fn get_marker(&self) -> String {
        format!("#{}#:", self.id.clone())
    }

    pub fn get_status(&self) -> SandboxStatus {
        self.status.clone()
    }

    /// Get the trajectory of commands executed in this sandbox
    pub fn get_trajectory(&self) -> &[CommandExecution] {
        &self.trajectory
    }

    /// Get the number of commands executed
    pub fn command_count(&self) -> usize {
        self.trajectory.len()
    }

    /// Get the last standalone command exit code
    pub fn get_last_standalone_exit_code(&self) -> Option<i64> {
        self.last_standalone_exit_code
    }

    /// Format the trajectory as a human-readable string
    pub fn format_trajectory(&self) -> String {
        let mut output = String::new();
        for cmd in self.trajectory.iter() {
            output.push_str(&format!("$ {}\n", cmd.command));

            match &cmd.result {
                Some(result) => {
                    if !result.output.is_empty() {
                        output.push_str(&result.output);
                        output.push('\n');
                    }
                }
                None => {
                    output.push_str("Status: Command started but no result recorded\n");
                }
            }
        }

        output
    }

    pub async fn stop(&mut self) -> Result<()> {
        // Release the semaphore
        self.permit.take();

        return match &self.status {
            SandboxStatus::Stopped(_) => Ok(()), // Already stopped
            SandboxStatus::Created => Err(SandboxError::NotStarted),
            SandboxStatus::Started(cid) => {
                // Stop the container but don't remove it
                let _ = self
                    .docker
                    .remove_container(
                        &cid,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
                self.status = SandboxStatus::Stopped(Ok(()));
                // Close input/output streams
                self.input = None;
                self.output_receiver = None;
                Ok(())
            }
        };
    }

    pub async fn start(&mut self, permit: OwnedSemaphorePermit) -> Result<()> {
        use bollard::query_parameters::{
            AttachContainerOptions, CreateContainerOptions, CreateImageOptions,
            StartContainerOptions,
        };
        use futures::TryStreamExt;

        match &self.status {
            SandboxStatus::Created => (),
            _ => return Err(SandboxError::AlreadyStarted),
        };

        // First, try to inspect the image to see if it exists locally
        match self.docker.inspect_image(&self.image).await {
            Ok(_) => {}
            Err(_) => {
                // Image doesn't exist locally, pull it
                let pull_options = Some(CreateImageOptions {
                    from_image: Some(self.image.clone()),
                    ..Default::default()
                });

                let mut pull_stream = self.docker.create_image(pull_options, None, None);
                while let Some(_) = pull_stream
                    .try_next()
                    .await
                    .map_err(|e| SandboxError::PullImageFailed(e.to_string()))?
                {
                    // TODO: print progress
                }
            }
        }

        let config = bollard::models::ContainerCreateBody {
            image: Some(self.image.clone()),
            cmd: Some(vec!["/bin/bash".to_string(), "-i".to_string()]),
            tty: Some(true),
            open_stdin: Some(true),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };

        let create_response = self
            .docker
            .create_container(None::<CreateContainerOptions>, config)
            .await
            .map_err(|e| SandboxError::StartContainerFailed(e.to_string()))?;

        self.status = SandboxStatus::Started(create_response.id.clone());

        self.docker
            .start_container(&create_response.id, None::<StartContainerOptions>)
            .await
            .map_err(|e| SandboxError::StartContainerFailed(e.to_string()))?;

        if !self.setup_commands.is_empty() {
            let CommandResult { output, exit_code } = self
                .exec_standalone_cmd(self.setup_commands.clone())
                .await?;
            if exit_code != 0 {
                return Err(SandboxError::SetupCommandsFailed(output));
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

        let attach_res = self
            .docker
            .attach_container(&create_response.id, Some(attach_options))
            .await
            .map_err(|e| SandboxError::StartContainerFailed(e.to_string()))?;

        let mut output_stream = attach_res.output;
        let input = attach_res.input;

        let (tx, rx) = futures::channel::mpsc::unbounded::<Bytes>();
        tokio::spawn(async move {
            while let Some(res) = output_stream.next().await {
                if let Ok(chunk) = res {
                    let bytes = match chunk {
                        LogOutput::StdOut { message } => message,
                        LogOutput::StdErr { message } => message,
                        LogOutput::Console { message } => message,
                        _ => continue,
                    };
                    let _ = tx.unbounded_send(bytes);
                } else {
                    break;
                }
            }
        });

        self.input = Some(Mutex::new(input));
        self.output_receiver = Some(Mutex::new(rx));

        {
            // `stty -echo` disables stdin from being echoed to the terminal.
            // `set +H` disables shell history expansion. (e.g. String containing `!!` will not expand to the last command)
            // `bind 'set enable-bracketed-paste off'` disables bracketed paste mode which adds a lot of noise to the output.
            // `PS1='{self.get_marker()}$?:'` sets the prompt to include the exit code of the last standalone command.
            // `PS2=''` disables the input prompt, should never be used anyway but just in case.
            let mut input_guard = self.input.as_ref().unwrap().lock().await;
            input_guard
                .write_all(format!("stty -echo; bind 'set enable-bracketed-paste off'; PS1='{}$?:'; PS2=''; readonly PS1; readonly PS2\n", self.get_marker()).as_bytes())
                .await
                .map_err(|e| SandboxError::ContainerWriteFailed(e.to_string()))?;
        }

        self.drain(0.5).await?;

        self.start_time = Some(Instant::now());
        self.permit = Some(permit);
        Ok(())
    }

    pub async fn exec_session_cmd(&mut self, cmd: String) -> Result<CommandResult> {
        match self.status.clone() {
            SandboxStatus::Started(_) => {
                // Record the command with timestamp
                let execution_start = Instant::now();
                let mut command_execution = CommandExecution {
                    command: cmd.clone(),
                    timestamp: execution_start,
                    result: None,
                };

                if command_execution.command.trim_start().starts_with('#') {
                    let result = CommandResult {
                        output: "".to_string(),
                        exit_code: 0,
                    };
                    command_execution.result = Some(result.clone());
                    self.trajectory.push(command_execution);
                    return Ok(result);
                }
                // Write raw command
                {
                    let mut input = self
                        .input
                        .as_ref()
                        .ok_or(SandboxError::NotStarted)?
                        .lock()
                        .await;
                    input
                        .write_all(format!("{}\n", cmd).as_bytes())
                        .await
                        .map_err(|e| {
                            error!("Error writing to container: {}", e);
                            SandboxError::ContainerWriteFailed(e.to_string())
                        })?;
                }

                // Read until prompt marker
                let output_raw = self.read_until_marker(&self.get_marker(), 20.0).await?;

                // Clean the output
                let cleaned = clean_terminal_output(&output_raw);

                // Find the position of the marker in cleaned output
                let marker_pos = cleaned.rfind(&self.get_marker()).unwrap_or(0);

                // Extract command output (before marker)
                let command_output = cleaned[..marker_pos].trim_end().to_string();

                // Extract prompt for exit code
                let prompt = &cleaned[marker_pos..];
                let exit_code = if let Some(code_str) = prompt.strip_prefix(&self.get_marker()).and_then(|s| s.strip_suffix(':')) {
                    code_str.trim().parse::<i64>().unwrap_or(-1)
                } else {
                    -1
                };

                let result = CommandResult {
                    output: command_output,
                    exit_code,
                };
                command_execution.result = Some(result.clone());
                self.trajectory.push(command_execution);

                // Drain any remaining output to next prompt
                self.drain(0.5).await?;

                Ok(result)
            }
            _ => return Err(SandboxError::NotStarted),
        }
    }

    pub async fn exec_standalone_cmd(&mut self, cmd: String) -> Result<CommandResult> {
        let cid = match &self.status {
            SandboxStatus::Started(cid) => cid,
            _ => return Err(SandboxError::NotStarted),
        };
        let exec_config = CreateExecOptions {
            cmd: Some(vec!["/bin/bash".to_string(), "-c".to_string(), cmd.clone()]),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            attach_stdin: Some(false),
            tty: Some(false),
            ..Default::default()
        };
        let exec = self
            .docker
            .create_exec(cid, exec_config)
            .await
            .map_err(|e| SandboxError::CreateExecFailed(e.to_string()))?;
        let start_res = self
            .docker
            .start_exec(&exec.id, None::<StartExecOptions>)
            .await
            .map_err(|e| SandboxError::CreateExecFailed(e.to_string()))?;
        let mut out = Vec::new();
        if let StartExecResults::Attached { output, .. } = start_res {
            let mut output = output;
            while let Some(item) = output.next().await {
                match item.map_err(|e| SandboxError::ContainerReadFailed(e.to_string()))? {
                    bollard::container::LogOutput::StdOut { message } => out.extend(&message),
                    bollard::container::LogOutput::StdErr { message } => out.extend(&message),
                    _ => {}
                }
            }
        }
        let inspect = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .map_err(|e| SandboxError::ContainerReadFailed(e.to_string()))?;
        let exit_code = inspect
            .exit_code
            .expect("Exit code not present in inspect exec");
        self.last_standalone_exit_code = Some(exit_code);
        let out_str = String::from_utf8_lossy(&out).to_string();
        Ok(CommandResult {
            output: out_str,
            exit_code,
        })
    }

    pub async fn drain(&mut self, timeout: f64) -> Result<String> {
        let receiver = self
            .output_receiver
            .as_ref()
            .ok_or(SandboxError::NotStarted)?;
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

    pub async fn read_until_marker(&mut self, marker: &str, timeout: f64) -> Result<String> {
        let receiver = self
            .output_receiver
            .as_ref()
            .ok_or(SandboxError::NotStarted)?;
        let mut receiver = receiver.lock().await;
        let mut accumulated = String::new();
        let start = Instant::now();
        while !strip_str(&accumulated).contains(marker) {
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed > timeout {
                return Err(SandboxError::TimeoutWaitingForMarker(marker.to_string()));
            }
            let remaining = timeout - elapsed;
            match time::timeout(Duration::from_secs_f64(remaining), receiver.next()).await {
                Ok(Some(chunk)) => accumulated += &String::from_utf8_lossy(&chunk),
                Ok(None) => {
                    return Err(SandboxError::ContainerReadFailed(
                        "Stream closed unexpectedly".to_string(),
                    ));
                }
                Err(_) => return Err(SandboxError::TimeoutWaitingForMarker(marker.to_string())),
            }
        }
        Ok(accumulated)
    }
}

fn clean_terminal_output(output: &str) -> String {
    let stripped = strip_str(output);
    let mut cleaned = String::new();
    for line in stripped.lines() {
        // Handle \r by taking the last segment after \r
        if let Some(last_part) = line.rsplit('\r').next() {
            cleaned.push_str(last_part);
            cleaned.push('\n');
        } else {
            cleaned.push_str(line);
            cleaned.push('\n');
        }
    }
    cleaned.trim_end().to_string()
}