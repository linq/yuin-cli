use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use uuid::Uuid;

use crate::domain::Snapshot;

pub const VERSION: u32 = 1;
pub const MAX_FRAME: usize = 1024 * 1024;
pub const MAX_QUEUE: usize = 500;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub request_id: Uuid,
    #[serde(flatten)]
    pub command: AppCommand,
}

impl Request {
    pub fn new(command: AppCommand) -> Self {
        Self {
            version: VERSION,
            request_id: Uuid::new_v4(),
            command,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AppCommand {
    Detached {
        command: Box<AppCommand>,
    },
    Service {
        command: crate::services::ServiceCommand,
    },
    Snapshot,
    Subscribe,
    Play {
        path: String,
    },
    PlayEntry {
        id: Uuid,
    },
    Pause,
    Resume,
    Stop,
    Next,
    Previous,
    Seek {
        seconds: f64,
    },
    Volume {
        percent: u8,
    },
    QueueAdd {
        path: String,
    },
    QueueRemove {
        id: Uuid,
    },
    QueueClear,
    Mode {
        mode: crate::domain::PlaybackMode,
    },
    QueueMove {
        id: Uuid,
        to: usize,
    },
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for RpcError {}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub request_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AppEvent {
    pub version: u32,
    pub sequence: u64,
    pub snapshot: Snapshot,
}

// read_line itself has no memory bound; limit every frame before allocating it.
pub async fn read_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> anyhow::Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    read_frame_buffered(reader, &mut frame).await
}

// Persist the partial frame when a select! branch cancels a subscription read.
pub async fn read_frame_buffered(
    reader: &mut (impl AsyncBufRead + Unpin),
    frame: &mut Vec<u8>,
) -> anyhow::Result<Option<Vec<u8>>> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            anyhow::ensure!(frame.is_empty(), "Truncated JSON frame");
            return Ok(None);
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(available.len(), |n| n + 1);
        anyhow::ensure!(frame.len() + count <= MAX_FRAME, "JSON frame exceeds 1 MiB");
        frame.extend_from_slice(&available[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(std::mem::take(frame)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncWriteExt, BufReader},
        time::{Duration, timeout},
    };

    #[tokio::test]
    async fn cancelling_a_subscription_read_preserves_the_partial_frame() {
        let (read, mut write) = tokio::io::duplex(64);
        let mut read = BufReader::new(read);
        let mut frame = Vec::new();
        write.write_all(b"{\"sequence\":").await.unwrap();
        assert!(
            timeout(
                Duration::from_millis(10),
                read_frame_buffered(&mut read, &mut frame)
            )
            .await
            .is_err()
        );
        write.write_all(b"42}\n").await.unwrap();
        let complete = read_frame_buffered(&mut read, &mut frame)
            .await
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&complete).unwrap();
        assert_eq!(value["sequence"], 42);
        assert!(frame.is_empty());
    }
}
