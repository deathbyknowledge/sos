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
        use uuid::Uuid;

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
            cmd: Some(shell::init_cmd()),
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
            let CommandResult { output, exit_code } = self
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
        use bollard::query_parameters::AttachContainerOptions;

        let container_id = match &self.status {
            SandboxStatus::Started(cid) => cid,
            _ => return Err(SandboxError::NotStarted),
        };

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
            .attach_container(&container_id.clone(), Some(attach_options))
            .await
            .map_err(|e| SandboxError::StartContainerFailed {
                message: e.to_string(),
                exit_code: None,
                logs: String::new(),
            })?;

        let mut output_stream = attach_res.output;
        let input = attach_res.input;

        // Spawn a task to forward the output stream to the channel
        let (tx, rx) = futures::channel::mpsc::unbounded::<Bytes>();
        tokio::spawn(async move {
            while let Some(res) = output_stream.next().await {
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

        {
            let mut input_guard = self.input.as_ref().unwrap().lock().await;
            input_guard
                .write_all(shell::conf_cmd(&self.get_marker()).as_bytes())
                .await
                .map_err(|e| SandboxError::ContainerWriteFailed(e.to_string()))?;
        }

        self.drain(0.5).await?;

        Ok(())
    }

    pub async fn exec_session_cmd(&mut self, cmd: String) -> Result<CommandResult> {
        match self.status {
            SandboxStatus::Started(_) => {
                let execution_start = Instant::now();
                let mut command_execution = CommandExecution {
                    command: cmd.clone(),
                    timestamp: execution_start,
                    result: None,
                };

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
                let output_raw = self.read_until_idle_after_marker(5.0, 0.5).await?;

                // Clean the output
                let cleaned = io::clean_terminal_output(&output_raw);

                // Use regex to find all markers, remove them, and get last exit code
                let marker_pattern = format!(r"{}(\d+):", regex::escape(&self.get_marker()));
                let marker_regex = regex::Regex::new(&marker_pattern)
                    .map_err(|e| SandboxError::ContainerReadFailed(e.to_string()))?;

                let mut last_exit_code = -1i64;
                let mut matches = marker_regex.captures_iter(&cleaned);
                while let Some(cap) = matches.next() {
                    if let Some(code_str) = cap.get(1) {
                        last_exit_code = code_str.as_str().parse::<i64>().unwrap_or(-1);
                    }
                }

                let cleaned_output = marker_regex.replace_all(&cleaned, "").to_string();
                let command_output = cleaned_output.trim_end().to_string();

                let result = CommandResult {
                    output: command_output,
                    exit_code: last_exit_code,
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
        })
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

    pub async fn drain(&mut self, timeout: f64) -> Result<String> {
        let receiver = self
            .output_receiver
            .as_ref()
            .ok_or(SandboxError::NotStarted)?;
        let mut receiver_guard = receiver.lock().await;
        let drained = io::drain(&mut receiver_guard, timeout).await;
        drained.map_err(|e| SandboxError::ContainerReadFailed(e.to_string()))
    }

    pub async fn read_until_idle_after_marker(
        &mut self,
        overall_timeout: f64,
        idle_timeout: f64,
    ) -> Result<String> {
        let receiver = self
            .output_receiver
            .as_ref()
            .ok_or(SandboxError::NotStarted)?;
        let mut receiver_guard = receiver.lock().await;
        let marker = self.get_marker();

        let result =
            io::read_stream_until_idle(&mut receiver_guard, &marker, overall_timeout, idle_timeout)
                .await;

        result.map_err(|e| SandboxError::ContainerReadFailed(e.to_string()))
    }
}
