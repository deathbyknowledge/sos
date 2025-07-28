use std::{pin::Pin, sync::Arc};

use bollard::{
    Docker,
    container::LogOutput,
    exec::{CreateExecOptions, StartExecOptions, StartExecResults},
    query_parameters::RemoveContainerOptions,
};
use bytes::Bytes;
use futures::{StreamExt, channel::mpsc::UnboundedReceiver};
use std::sync::atomic::{AtomicU32, Ordering};
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::time::{self, Duration, Instant};
use tokio::{io::AsyncWriteExt, sync::OwnedSemaphorePermit};

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
            SandboxStatus::Started(container_id) => {
                write!(f, "started (container id: {})", container_id)
            }
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
    pub stdout: String,
    pub stderr: String,
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
    command_id: AtomicU32,
    docker: Arc<Docker>,
    trajectory: Vec<CommandExecution>,
}

impl Sandbox {
    pub fn new(id: String, image: String, setup_commands: String, docker: Arc<Docker>) -> Self {
        Sandbox {
            id,
            image,
            setup_commands,
            docker,
            command_id: AtomicU32::new(0),
            status: SandboxStatus::Created,
            permit: None,
            input: None,
            output_receiver: None,
            start_time: None,
            trajectory: Vec::new(),
        }
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

    /// Get the latest command execution
    pub fn get_latest_command(&self) -> Option<&CommandExecution> {
        self.trajectory.last()
    }

    /// Get a specific command by index
    pub fn get_command(&self, index: usize) -> Option<&CommandExecution> {
        self.trajectory.get(index)
    }

    /// Get all successful commands (exit code 0)
    pub fn get_successful_commands(&self) -> Vec<&CommandExecution> {
        self.trajectory
            .iter()
            .filter(|cmd| cmd.result.as_ref().map_or(false, |r| r.exit_code == 0))
            .collect()
    }

    /// Get all failed commands (exit code != 0)
    pub fn get_failed_commands(&self) -> Vec<&CommandExecution> {
        self.trajectory
            .iter()
            .filter(|cmd| cmd.result.as_ref().map_or(true, |r| r.exit_code != 0))
            .collect()
    }

    /// Format the trajectory as a human-readable string
    pub fn format_trajectory(&self) -> String {
        let mut output = String::new();
        for cmd in self.trajectory.iter() {
            output.push_str(&format!("$ {}\n", cmd.command));

            match &cmd.result {
                Some(result) => {
                    if !result.stdout.is_empty() {
                        output.push_str(&result.stdout);
                        output.push('\n');
                    }
                    if !result.stderr.is_empty() {
                        output.push_str(&result.stderr);
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
            tty: Some(false),
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
            let CommandResult {
                stderr,
                exit_code,
                stdout: _,
            } = self
                .exec_standalone_cmd(self.setup_commands.clone())
                .await?;
            if exit_code != 0 {
                return Err(SandboxError::SetupCommandsFailed(stderr));
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
            let mut input_guard = self.input.as_ref().unwrap().lock().await;
            input_guard
                .write_all("stty -echo; set +H\n".as_bytes())
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
            SandboxStatus::Started(cid) => {
                // Record the command with timestamp
                let execution_start = Instant::now();
                let mut command_execution = CommandExecution {
                    command: cmd.clone(),
                    timestamp: execution_start,
                    result: None,
                };

                if command_execution.command.trim_start().starts_with('#') {
                    let result = CommandResult {
                        stdout: "".to_string(),
                        stderr: "".to_string(),
                        exit_code: 0,
                    };
                    command_execution.result = Some(result.clone());
                    self.trajectory.push(command_execution);
                    return Ok(result);
                }

                let command_id = self.command_id.fetch_add(1, Ordering::Relaxed);
                let stdout_file = format!("/tmp/stdout_{}.txt", command_id);
                let stderr_file = format!("/tmp/stderr_{}.txt", command_id);
                let exitcode_file = format!("/tmp/exitcode_{}.txt", command_id);
                let marker = format!("COMMAND_DONE_{}", command_id);

                let grouped_command = format!("{{ {} ; }}", cmd);
                let cmd_to_send = format!(
                    "{} > {} 2> {}; echo $? > {}; echo '{}'\n",
                    grouped_command, stdout_file, stderr_file, exitcode_file, marker
                );

                {
                    let mut input = self
                        .input
                        .as_ref()
                        .ok_or(SandboxError::NotStarted)?
                        .lock()
                        .await;
                    input
                        .write_all(cmd_to_send.as_bytes())
                        .await
                        .map_err(|e| SandboxError::ContainerWriteFailed(e.to_string()))?;
                }

                self.read_until_marker(&marker, 20.0).await?;

                // Read all three files in a single exec command with delimiters
                let combined_cmd = format!(
                    "echo 'STDOUT_START'; cat {}; echo 'STDOUT_END'; echo 'STDERR_START'; cat {}; echo 'STDERR_END'; echo 'EXITCODE_START'; cat {}; echo 'EXITCODE_END'",
                    stdout_file, stderr_file, exitcode_file
                );

                let CommandResult {
                    stdout: combined_output,
                    stderr: _,
                    exit_code: exec_exit_code,
                } = self.exec_standalone_cmd(combined_cmd).await?;

                if exec_exit_code != 0 {
                    return Err(SandboxError::ExecFailed(combined_output, exec_exit_code));
                }

                // Parse the combined output
                let stdout = extract_section(&combined_output, "STDOUT_START", "STDOUT_END")
                    .trim_end_matches('\n')
                    .to_string();
                let stderr = extract_section(&combined_output, "STDERR_START", "STDERR_END")
                    .trim_end_matches('\n')
                    .to_string();
                let exit_code_str =
                    extract_section(&combined_output, "EXITCODE_START", "EXITCODE_END")
                        .trim()
                        .to_string();

                // Since we're non-tty, stderr is prefixed by `bash: `, so we need to remove that
                let stderr = stderr.trim_start_matches("bash: ").to_string();

                let exit_code = exit_code_str.parse::<i64>().unwrap_or(-1);

                // Store the result in trajectory
                let result = CommandResult {
                    stdout,
                    stderr,
                    exit_code,
                };
                command_execution.result = Some(result.clone());
                self.trajectory.push(command_execution);

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

                let exec = self
                    .docker
                    .create_exec(&cid, exec_config)
                    .await
                    .map_err(|e| SandboxError::CreateExecFailed(e.to_string()))?;

                self.docker
                    .start_exec(
                        &exec.id,
                        Some(StartExecOptions {
                            detach: true,
                            tty: false,
                            ..Default::default()
                        }),
                    )
                    .await
                    .map_err(|e| SandboxError::CreateExecFailed(e.to_string()))?;

                self.drain(0.5).await?;
                

                return Ok(result);
            }
            _ => return Err(SandboxError::NotStarted),
        }
    }

    pub async fn exec_standalone_cmd(&self, cmd: String) -> Result<CommandResult> {
        let cid = match &self.status {
            SandboxStatus::Started(cid) => cid,
            _ => return Err(SandboxError::NotStarted),
        };
        let exec_config = CreateExecOptions {
            cmd: Some(vec!["/bin/bash".to_string(), "-c".to_string(), cmd]),
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
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        if let StartExecResults::Attached { output, .. } = start_res {
            let mut output = output;
            while let Some(item) = output.next().await {
                match item.map_err(|e| SandboxError::ContainerReadFailed(e.to_string()))? {
                    bollard::container::LogOutput::StdOut { message } => stdout.extend(&message),
                    bollard::container::LogOutput::StdErr { message } => stderr.extend(&message),
                    _ => {}
                }
            }
        }
        let inspect = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .map_err(|e| SandboxError::ContainerReadFailed(e.to_string()))?;
        let exit_code = inspect.exit_code.unwrap_or(-1);
        let stdout_str = String::from_utf8_lossy(&stdout).to_string();
        let stderr_str = String::from_utf8_lossy(&stderr).to_string();
        Ok(CommandResult {
            stdout: stdout_str,
            stderr: stderr_str,
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

    pub async fn read_until_marker(&mut self, marker: &str, timeout: f64) -> Result<()> {
        let receiver = self
            .output_receiver
            .as_ref()
            .ok_or(SandboxError::NotStarted)?;
        let mut receiver = receiver.lock().await;
        let mut accumulated = String::new();
        let start = Instant::now();
        while !accumulated.contains(marker) {
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
        Ok(())
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
