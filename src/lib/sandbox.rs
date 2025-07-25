use std::{pin::Pin, sync::Arc};

use anyhow::{Result, anyhow};
use bollard::{
    exec::{CreateExecOptions, StartExecOptions, StartExecResults}, query_parameters::RemoveContainerOptions, Docker
};
use bytes::Bytes;
use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::Mutex;
use tokio::time::{self, Duration, Instant};
use tokio::{io::AsyncWriteExt, sync::OwnedSemaphorePermit};

pub struct Sandbox {
    pub id: String,
    pub image: String,
    pub setup_commands: String,
    pub container_id: Option<String>,
    pub permit: Option<tokio::sync::OwnedSemaphorePermit>,
    pub input: Option<Mutex<Pin<Box<dyn tokio::io::AsyncWrite + Send>>>>,
    pub output_receiver: Option<Mutex<UnboundedReceiver<Bytes>>>,
    pub start_time: Option<Instant>,
    command_id: AtomicU32,
    docker: Arc<Docker>,
}

impl Sandbox {
    pub fn new(id: String, image: String, setup_commands: String, docker: Arc<Docker>) -> Self {
        Sandbox {
            id,
            image,
            setup_commands,
            docker,
            command_id: AtomicU32::new(0),
            container_id: None,
            permit: None,
            input: None,
            output_receiver: None,
            start_time: None,
        }
    }

    pub async fn stop(&mut self) -> Result<()> {
        let cid_opt = self.container_id.clone();

        if let Some(cid) = cid_opt {
            let _ = self
                .docker
                .remove_container(
                    &cid,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await?;
        } else {
            return Err(anyhow!("Sandbox not started"));
        }

        // Sandbox_arc drops here, releasing permit if held
        Ok(())
    }

    pub async fn start(&mut self, permit: OwnedSemaphorePermit) -> Result<()> {
        if self.container_id.is_some() {
            return Err(anyhow!("Sandbox already started"));
        }
        // Check if image exists locally, pull if it doesn't
        use bollard::query_parameters::CreateImageOptions;
        use futures::TryStreamExt;

        // First, try to inspect the image to see if it exists locally
        match self.docker.inspect_image(&self.image).await {
            Ok(_) => {
                // Image exists locally, no need to pull
            }
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
            .create_container(
                None::<bollard::query_parameters::CreateContainerOptions>,
                config,
            )
            .await?;

        self.container_id = Some(create_response.id.clone());

        self.docker
            .start_container(
                &create_response.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await?;

        if !self.setup_commands.is_empty() {
            let (_, _, exit_code) = self.exec_standalone_cmd(self.setup_commands.split_whitespace().map(|s| s.to_string()).collect()).await?;
            if exit_code != 0 {
                return Err(anyhow!("Setup commands failed"));
            }
        }

        let attach_options = bollard::query_parameters::AttachContainerOptions {
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
            .await?;

        let mut output_stream = attach_res.output;
        let input = attach_res.input;

        let (tx, rx) = futures::channel::mpsc::unbounded::<Bytes>();
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

        self.input = Some(Mutex::new(input));
        self.output_receiver = Some(Mutex::new(rx));

        {
            // `stty -echo` disables stdin from being echoed to the terminal.
            // `set +H` disables shell history expansion. (e.g. String containing `!!` will not expand to the last command)
            let mut input_guard = self.input.as_ref().unwrap().lock().await;
            input_guard
                .write_all("stty -echo; set +H\n".as_bytes())
                .await?;
        }

        self.drain(0.5).await?;

        self.start_time = Some(Instant::now());
        self.permit = Some(permit);
        Ok(())
    }

    pub async fn exec_session_cmd(
        &mut self,
        cmd: String,
    ) -> Result<(String, String, i64), anyhow::Error> {
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
                .ok_or(anyhow!("Sandbox not started"))?
                .lock()
                .await;
            input.write_all(cmd_to_send.as_bytes()).await?;
        }

        self.read_until_marker(&marker, 20.0).await?;

        // Read all three files in a single exec command with delimiters
        let combined_cmd = vec![
            "/bin/bash".to_string(),
            "-c".to_string(),
            format!(
                "echo 'STDOUT_START'; cat {}; echo 'STDOUT_END'; echo 'STDERR_START'; cat {}; echo 'STDERR_END'; echo 'EXITCODE_START'; cat {}; echo 'EXITCODE_END'",
                stdout_file, stderr_file, exitcode_file
            ),
        ];

        let (combined_output, _, exec_exit_code) = self.exec_standalone_cmd(combined_cmd).await?;

        if exec_exit_code != 0 {
            return Err(anyhow!("Failed to read output files"));
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

        let cid = self.container_id.as_ref().unwrap();
        let exec = self.docker.create_exec(cid, exec_config).await?;

        self.docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    tty: false,
                    ..Default::default()
                }),
            )
            .await?;

        self.drain(0.5).await?;

        return Ok((stdout, stderr, exit_code_str.parse::<i64>().unwrap_or(-1)));
    }

    pub async fn exec_standalone_cmd(&self, cmd: Vec<String>) -> Result<(String, String, i64)> {
        let cid = self.container_id.as_ref().ok_or(anyhow!("No container"))?;
        let exec_config = CreateExecOptions {
            cmd: Some(cmd),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            attach_stdin: Some(false),
            tty: Some(false),
            ..Default::default()
        };
        let exec = self.docker.create_exec(cid, exec_config).await?;
        let start_res = self
            .docker
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
        let inspect = self.docker.inspect_exec(&exec.id).await?;
        let exit_code = inspect.exit_code.unwrap_or(-1);
        let stdout_str = String::from_utf8_lossy(&stdout).to_string();
        let stderr_str = String::from_utf8_lossy(&stderr).to_string();
        Ok((stdout_str, stderr_str, exit_code))
    }

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
