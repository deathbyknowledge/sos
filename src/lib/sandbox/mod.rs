mod io;
mod shell;
pub mod types;

use std::{pin::Pin, sync::Arc, time::Duration};
pub use types::{
    CommandExecution, CommandResult, Error as SandboxError, Result, Status as SandboxStatus,
};

use bollard::{
    Docker,
    container::LogOutput,
    exec::{CreateExecOptions, StartExecOptions, StartExecResults},
    query_parameters::RemoveContainerOptions,
};
use bytes::Bytes;
use futures::{StreamExt, channel::mpsc::UnboundedReceiver};
use tokio::sync::Mutex;
use tokio::time::Instant;
use tokio::{io::AsyncWriteExt, sync::OwnedSemaphorePermit};
use tracing::error;
pub struct Sandbox {
    /// UUID for the sandbox
    pub id: String,
    /// Docker image to use for the sandbox container
    pub image: String,
    /// Commands to run on startup
    pub setup_commands: String,
    /// Instant when the sandbox and container were started
    pub start_time: Option<Instant>,
    /// Current status of the sandbox
    status: SandboxStatus,
    /// Semaphore permit for the sandbox. Used to limit the number of concurrent sandboxes.
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    /// Input stream for the sandbox (stdin)
    input: Option<Mutex<Pin<Box<dyn tokio::io::AsyncWrite + Send>>>>,
    /// Output stream for the sandbox (stdout/stderr)
    output_receiver: Option<Mutex<UnboundedReceiver<Bytes>>>,
    /// Docker client
    docker: Arc<Docker>,
    /// Trajectory of commands executed in the sandbox
    trajectory: Vec<CommandExecution>,
    /// Last standalone command exit code
    last_standalone_exit_code: Option<i64>,
}

impl Sandbox {
    pub fn new(image: String, setup_commands: String, docker: Arc<Docker>) -> Self {
        use uuid::Uuid;

        let id = Uuid::new_v4().to_string();

        Sandbox {
            id,
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

    pub fn get_status(&self) -> &SandboxStatus {
        &self.status
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

    pub async fn start(&mut self, permit: OwnedSemaphorePermit) -> Result<()> {
        if !matches!(self.status, SandboxStatus::Created) {
            return Err(SandboxError::AlreadyStarted);
        }

        self.pull_image_if_missing().await?;
        let container_id = self.create_and_start_container().await?;
        self.status = SandboxStatus::Started(container_id.clone());

        // Run initial shell setup
        self.run_setup_commands().await?;
        self.attach_and_configure_shell().await?;

        self.start_time = Some(Instant::now());
        self.permit = Some(permit);
        Ok(())
    }

    async fn pull_image_if_missing(&mut self) -> Result<()> {
        use bollard::query_parameters::CreateImageOptions;
        use futures::TryStreamExt;

        match self.docker.inspect_image(&self.image).await {
            Ok(_) => Ok(()),
            Err(_) => {
                // Image doesn't exist locally, pull it
                let pull_options = Some(CreateImageOptions {
                    from_image: Some(self.image.clone()),
                    ..Default::default()
                });

                let mut pull_stream = self.docker.create_image(pull_options, None, None);
                while let Some(_) = pull_stream.try_next().await? {
                    // TODO: print progress
                }
                return Ok(());
            }
        }
    }

    async fn create_and_start_container(&mut self) -> Result<String> {
        use bollard::query_parameters::{
            CreateContainerOptions, InspectContainerOptions, LogsOptions, StartContainerOptions,
        };
        use bollard::secret::ContainerStateStatusEnum;

        let config = bollard::models::ContainerCreateBody {
            image: Some(self.image.clone()),
            cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
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
            .map_err(|e| SandboxError::StartContainerFailed {
                message: e.to_string(),
                exit_code: None,
                logs: String::new(),
            })?;

        self.status = SandboxStatus::Started(create_response.id.clone());

        self.docker
            .start_container(&create_response.id, None::<StartContainerOptions>)
            .await
            .map_err(|e| SandboxError::StartContainerFailed {
                message: e.to_string(),
                exit_code: None,
                logs: String::new(),
            })?;
        let mut attempts = 0;
        let max_attempts = 6; // ~3 seconds at 500ms intervals
        let container_id = create_response.id.clone();
        loop {
            let inspect = self
                .docker
                .inspect_container(&container_id, None::<InspectContainerOptions>)
                .await
                .map_err(|e| SandboxError::StartContainerFailed {
                    message: format!("Failed to inspect container: {}", e),
                    exit_code: None,
                    logs: String::new(),
                })?;

            if inspect.state.as_ref().and_then(|s| s.running) == Some(true) {
                break; // Success
            }

            if attempts >= max_attempts {
                // Fetch logs for diagnostics
                let mut log_stream = self.docker.logs(
                    &container_id,
                    Some(LogsOptions {
                        stdout: true,
                        stderr: true,
                        tail: "all".to_string(),
                        ..Default::default()
                    }),
                );

                let mut logs = String::new();
                while let Some(item) = log_stream.next().await {
                    match item.map_err(|e| SandboxError::ContainerReadFailed(e.to_string()))? {
                        LogOutput::StdOut { message } => logs += &String::from_utf8_lossy(&message),
                        LogOutput::StdErr { message } => logs += &String::from_utf8_lossy(&message),
                        _ => {}
                    }
                }

                let exit_code = inspect.state.clone().and_then(|s| s.exit_code);
                let error_msg = inspect
                    .state
                    .clone()
                    .and_then(|s| s.error.clone())
                    .unwrap_or_default();
                let status = inspect
                    .state
                    .and_then(|s| s.status.clone())
                    .unwrap_or(ContainerStateStatusEnum::EMPTY);

                error!(
                    "Container {} failed to start. Status: {}, Exit code: {:?}, Error: {}, Logs: {}",
                    container_id, status, exit_code, error_msg, logs
                );

                return Err(SandboxError::StartContainerFailed {
                    message: format!(
                        "Container exited immediately. Status: {}, Error: {}",
                        status, error_msg
                    ),
                    exit_code,
                    logs,
                });
            }

            attempts += 1;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        Ok(create_response.id)
    }

    async fn run_setup_commands(&mut self) -> Result<()> {
        if !self.setup_commands.is_empty() {
            let CommandResult { output, exit_code, exited: _ } = self
                .exec_standalone_cmd(self.setup_commands.clone())
                .await?;
            if exit_code != 0 {
                error!(
                    "Setup commands ({}) failed: {}",
                    self.setup_commands, output
                );
                return Err(SandboxError::SetupCommandsFailed(output));
            }
        }
        Ok(())
    }

    async fn attach_and_configure_shell(&mut self) -> Result<()> {
        let container_id = match &self.status {
            SandboxStatus::Started(cid) => cid,
            _ => return Err(SandboxError::NotStarted),
        };

        let create_exec_res = self
            .docker
            .create_exec(
                container_id,
                CreateExecOptions {
                    cmd: Some(shell::init_cmd()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    attach_stdin: Some(true),
                    tty: Some(true),
                    ..Default::default()
                },
            )
            .await?;

        let start_exec_res = self
            .docker
            .start_exec(
                &create_exec_res.id,
                Some(StartExecOptions {
                    detach: false,
                    tty: true,
                    ..Default::default()
                }),
            )
            .await?;
        let (mut output, input) = match start_exec_res {
            StartExecResults::Attached { output, input, .. } => (output, input),
            _ => {
                return Err(SandboxError::StartContainerFailed {
                    message: "Failed to start exec, didn't attach.".to_string(),
                    exit_code: None,
                    logs: String::new(),
                });
            }
        };

        // Spawn a task to forward the output stream to the channel
        let (tx, rx) = futures::channel::mpsc::unbounded::<Bytes>();
        tokio::spawn(async move {
            while let Some(res) = output.next().await {
                if let Ok(chunk) = res {
                    let bytes = match chunk {
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

        self.write_cmd(shell::CONF_CMD.to_string()).await?;

        let _ = self.read_until_idle_after_marker(2.0, 0.1, 1).await?;
        Ok(())
    }

    pub async fn exec_session_cmd(&mut self, cmd: String) -> Result<CommandResult> {
        let cid = match &self.status {
            SandboxStatus::Started(cid) => cid.clone(),
            SandboxStatus::Exited(_) => return Err(SandboxError::AlreadyExited),
            _ => return Err(SandboxError::NotStarted),
        };

        let execution_start = Instant::now();
        let mut command_execution = CommandExecution {
            command: cmd.clone(),
            timestamp: execution_start,
            result: None,
        };

        // Write raw command
        self.write_cmd(format!("{}\n", &cmd)).await?;

        // Hint how many commands were executed by counting the number of newlines present.
        // Might not be an exact match but it allows us to cut the timeout short.
        let n_commands_hint = cmd.split('\n').count();
        let output = match self
            .read_until_idle_after_marker(2.0, 0.2, n_commands_hint)
            .await
        {
            Ok(s) => s,
            Err(SandboxError::TimeoutWaitingForMarker(_)) => {
                // Step 1: try a newline to complete open constructs
                self.write_cmd("\n".to_string()).await?;
                match self.read_until_idle_after_marker(2.0, 0.2, 1).await {
                    Ok(s2) => s2,
                    Err(SandboxError::TimeoutWaitingForMarker(_)) => {
                        // Step 2: try Ctrl-D (safe due to 'set -o ignoreeof')
                        self.write_cmd("\x04".to_string()).await?;
                        // Final attempt to reach PS1
                        self.read_until_idle_after_marker(2.0, 0.2, 1).await?
                    }
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        };

        // Find all markers, remove them, and get last exit code (if input included multiple commands)
        let (output, exit_code, exit_marker_seen) =
            io::strip_markers_and_extract_exit_code(&output);

        // Session was terminated by a command.
        if exit_marker_seen {
            self.status = SandboxStatus::Exited(cid.clone());
        }

        let result = CommandResult { output, exit_code, exited: exit_marker_seen };
        command_execution.result = Some(result.clone());
        self.trajectory.push(command_execution);

        // Drain any remaining output to next prompt

        Ok(result)
    }

    pub async fn exec_standalone_cmd(&mut self, cmd: String) -> Result<CommandResult> {
        let cid = match &self.status {
            SandboxStatus::Started(cid) | SandboxStatus::Exited(cid) => cid,
            _ => return Err(SandboxError::NotStarted),
        };
        let exec_config = CreateExecOptions {
            cmd: Some(shell::standalone_cmd(&cmd)),
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
                    LogOutput::StdOut { message } => out.extend(&message),
                    LogOutput::StdErr { message } => out.extend(&message),
                    _ => continue,
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
            exited: false
        })
    }

    pub async fn stop(&mut self) -> Result<()> {
        // Release the semaphore
        self.permit.take();

        return match &self.status {
            SandboxStatus::Stopped(_) => Err(SandboxError::NotStarted), // Already stopped
            SandboxStatus::Created => Err(SandboxError::NotStarted),
            SandboxStatus::Started(cid) | SandboxStatus::Exited(cid) => {
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

    async fn write_cmd(&mut self, cmd: String) -> Result<()> {
        let mut input = self
            .input
            .as_ref()
            .ok_or(SandboxError::NotStarted)?
            .lock()
            .await;
        input
            .write_all(cmd.as_bytes())
            .await
            .map_err(|e| SandboxError::ContainerWriteFailed(e.to_string()))?;
        input
            .flush()
            .await
            .map_err(|e| SandboxError::ContainerWriteFailed(e.to_string()))?;
        Ok(())
    }

    async fn read_until_idle_after_marker(
        &mut self,
        overall_timeout: f64,
        idle_timeout: f64,
        short_circuit_after_n_markers: usize,
    ) -> Result<String> {
        let receiver = self
            .output_receiver
            .as_ref()
            .ok_or(SandboxError::NotStarted)?;
        let mut receiver_guard = receiver.lock().await;

        let result = io::read_stream_until_idle(
            &mut receiver_guard,
            overall_timeout,
            idle_timeout,
            short_circuit_after_n_markers,
        )
        .await;
        match result {
            Ok(s) => Ok(s),
            Err(io::ReadError::OverallTimeout) => Err(SandboxError::TimeoutWaitingForMarker(
                "Marker not seen before timeout (possible incomplete input)".to_string(),
            )),
            Err(io::ReadError::StreamClosed) => Err(SandboxError::ContainerReadFailed(
                "Stream closed unexpectedly".to_string(),
            )),
        }
    }
}
