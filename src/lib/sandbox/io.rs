use super::shell::{PS1_MARKER, PS2_MARKER};
use bytes::Bytes;
use futures::{StreamExt, channel::mpsc::UnboundedReceiver};
use strip_ansi_escapes::strip_str;
use thiserror::Error;
use tokio::time::{self, Duration, Instant, sleep};

#[derive(Error, Debug)]
pub enum ReadError {
    #[error("Overall timeout reached while waiting for output")]
    OverallTimeout,
    #[error("Stream closed unexpectedly")]
    StreamClosed,
}

pub async fn read_stream_until_idle(
    receiver: &mut UnboundedReceiver<Bytes>,
    overall_timeout: f64,
    idle_timeout: f64,
    short_circuit_after_n_markers: usize,
) -> Result<String, ReadError> {
    let mut accumulated = String::new();
    let start = Instant::now();

    let mut markers_seen = 0;
    loop {
        if start.elapsed().as_secs_f64() > overall_timeout {
            return Err(ReadError::OverallTimeout);
        }

        match time::timeout(Duration::from_secs_f64(idle_timeout), receiver.next()).await {
            Ok(Some(chunk)) => {
                let new_chunk = strip_str(String::from_utf8_lossy(&chunk).to_string());
                accumulated += &new_chunk;
                // We can't just naively check for markers and break early here as we could have multiple outputs
                // split across multiple chunks. This normally happens if the command was multiline. To avoid
                // having to rely on the idle timeout only to check for markers, we use the number of newlines
                // in the input command as a hint to how many ouputs we should expect.
                if OUTPUT_MARKER_REGEX.is_match(&accumulated) {
                    markers_seen += 1;
                    if markers_seen >= short_circuit_after_n_markers {
                        break;
                    }
                }
            }
            Ok(None) => {
                println!("Stream closed in read_stream_until_idle");
                return Err(ReadError::StreamClosed);
            }
            Err(_) => {
                // Idle timeout
                if OUTPUT_MARKER_REGEX.is_match(&accumulated) {
                    break;
                }
                // Micro-poll for quick checks
                sleep(Duration::from_millis(10)).await;
            }
        }
    }
    Ok(accumulated)
}

pub fn strip_markers_and_extract_exit_code(output: &str) -> (String, i64) {
    let mut last_exit_code = -1i64;
    // First remove PS2 markers
    let mut cleaned = output.replace(&PS2_MARKER, "").to_string();

    // Then strip output marker (PS1) and extract the exit code
    let mut matches = OUTPUT_MARKER_REGEX.captures_iter(&cleaned);
    while let Some(cap) = matches.next() {
        if let Some(code_str) = cap.get(1) {
            last_exit_code = code_str
                .as_str()
                .parse::<i64>()
                .expect("Failed to parse exit code");
        }
    }

    cleaned = OUTPUT_MARKER_REGEX.replace_all(&cleaned, "").to_string();
    cleaned = cleaned.replace(&PS1_MARKER, "");

    cleaned = cleaned.trim_end().to_string();
    (cleaned, last_exit_code)
}

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    pub static ref OUTPUT_MARKER_REGEX: Regex = {
        let pattern = format!(r"{}(\d+):", regex::escape(PS1_MARKER));
        Regex::new(&pattern).expect("Invalid PS1 marker regex")
    };
}
