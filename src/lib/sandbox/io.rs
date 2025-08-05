use bytes::Bytes;
use futures::{StreamExt, channel::mpsc::UnboundedReceiver};
use strip_ansi_escapes::strip_str;
use thiserror::Error;
use tokio::time::{self, Duration, Instant};

#[derive(Error, Debug)]
pub enum ReadError {
    #[error("Overall timeout reached while waiting for output")]
    OverallTimeout,
    #[error("Stream closed unexpectedly")]
    StreamClosed,
}

pub async fn read_stream_until_idle(
    receiver: &mut UnboundedReceiver<Bytes>,
    marker: &str,
    overall_timeout: f64,
    idle_timeout: f64,
) -> Result<String, ReadError> {
    let mut accumulated = String::new();
    let start = Instant::now();

    // All the complex loop logic lives here now.
    loop {
        if start.elapsed().as_secs_f64() > overall_timeout {
            return Err(ReadError::OverallTimeout);
        }

        match time::timeout(Duration::from_secs_f64(idle_timeout), receiver.next()).await {
            Ok(Some(chunk)) => {
                accumulated += &String::from_utf8_lossy(&chunk);
            }
            Ok(None) => return Err(ReadError::StreamClosed),
            Err(_) => {
                // Idle timeout
                if strip_str(&accumulated).contains(marker) {
                    break;
                }
            }
        }
    }

    Ok(accumulated)
}

pub async fn drain(receiver: &mut UnboundedReceiver<Bytes>, timeout: f64) -> Result<String, ReadError> {
    let mut drained: Vec<u8> = Vec::new();
    loop {
        match time::timeout(Duration::from_secs_f64(timeout), receiver.next()).await {
            Ok(Some(b)) => drained.extend(&b),
            Ok(None) => return Err(ReadError::StreamClosed),
            Err(_) => break,
        }
    }
    Ok(String::from_utf8_lossy(&drained).to_string())
}

pub fn clean_terminal_output(output: &str) -> String {
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