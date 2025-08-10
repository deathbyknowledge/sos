use thiserror::Error;
use tokio::time::Instant;
use tracing::error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Sandbox not started")]
    NotStarted,
    #[error("Sandbox already started")]
    AlreadyStarted,
    #[error("Sandbox session already exited")]
    AlreadyExited,
    #[error("Setup commands failed: {0}")]
    SetupCommandsFailed(String),
    #[error("Failed to pull image")]
    PullImageFailed {
        #[from]
        source: bollard::errors::Error,
    },
    #[error("Failed to stop container: {0}")]
    StopContainerFailed(String),
    #[error("Failed to start container: {message}. Exit code: {exit_code:?}, Logs: {logs}")]
    StartContainerFailed {
        message: String,
        exit_code: Option<i64>,
        logs: String,
    },
    #[error("Container write failed")]
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

// TODO: capture exit code on exit command
#[derive(Debug)]
pub enum Status {
    Created,
    Started(String),     // container id
    Exited(String), // Session exited but container is still running
    Stopped(Result<()>), // result of stop
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Created => write!(f, "created"),
            Status::Started(_) => write!(f, "started"),
            Status::Exited(_) => write!(f, "exited"),
            Status::Stopped(_) => write!(f, "stopped"),
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
    pub exited: bool,
}
