//! mpv owns decoding; queue identity and playback generations belong to the core.
use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::os::{fd::OwnedFd, unix::net::UnixStream as StdUnixStream};

use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::{UnixStream, unix::OwnedWriteHalf},
    process::{Child, Command},
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};

use crate::protocol::read_frame;

#[derive(Debug)]
pub enum BackendEvent {
    Loaded,
    Position(f64),
    Duration(f64),
    Pause(bool),
    Buffering(bool),
    Ended,
    Failed(String),
}

pub struct Event {
    pub generation: u64,
    pub event: BackendEvent,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

pub struct Mpv {
    child: Child,
    writer: OwnedWriteHalf,
    pending: Pending,
    next_id: AtomicU64,
    reader: JoinHandle<()>,
    request_timeout: Duration,
}

impl Mpv {
    pub async fn load(
        path: &str,
        headers: &std::collections::BTreeMap<String, String>,
        position: f64,
        volume: u8,
        generation: u64,
        events: mpsc::Sender<Event>,
    ) -> Result<Self> {
        // An inherited socketpair is private and makes mpv exit even if the daemon
        // is killed: mpv's input-ipc-client exits when its peer closes.
        let (parent, child_socket) = StdUnixStream::pair()?;
        parent.set_nonblocking(true)?;
        let stream = UnixStream::from_std(parent)?;
        let binary = std::env::var_os("YUIN_MPV").unwrap_or_else(|| "mpv".into());
        let child = Command::new(binary)
            .args([
                "--no-config",
                "--load-scripts=no",
                "--input-terminal=no",
                "--input-default-bindings=no",
                "--input-media-keys=no",
                "--terminal=no",
                "--vid=no",
                "--audio-display=no",
                "--idle=yes",
                "--keep-open=no",
                "--pause=yes",
                "--ytdl=no",
                "--autoload-files=no",
                "--access-references=no",
            ])
            .arg("--input-ipc-client=fd://0")
            .arg(format!("--volume={volume}"))
            .arg(format!("--start={position}"))
            .stdin(Stdio::from(OwnedFd::from(child_socket)))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("Cannot start mpv; install mpv or set YUIN_MPV to its executable")?;
        let (read, writer) = stream.into_split();
        let pending: Pending = Arc::default();
        let replies = pending.clone();
        let (ready_tx, ready_rx) = oneshot::channel();
        let reader = tokio::spawn(async move {
            let mut ready = Some(ready_tx);
            let mut read = BufReader::new(read);
            let result = async {
                while let Some(frame) = read_frame(&mut read).await? {
                    let value: Value = serde_json::from_slice(&frame)?;
                    if let Some(id) = value["request_id"].as_u64() {
                        if let Some(reply) = replies.lock().await.remove(&id) {
                            let result = if value["error"] == "success" {
                                Ok(value["data"].clone())
                            } else {
                                Err(format!("mpv command failed: {}", value["error"]))
                            };
                            let _ = reply.send(result);
                        }
                        continue;
                    }
                    let event = match value["event"].as_str() {
                        Some("file-loaded") => {
                            if let Some(ready) = ready.take() {
                                let _ = ready.send(Ok(()));
                            }
                            Some(BackendEvent::Loaded)
                        }
                        Some("end-file") => match value["reason"].as_str() {
                            Some("eof") => Some(BackendEvent::Ended),
                            Some("error") => {
                                if let Some(ready) = ready.take() {
                                    let _ =
                                        ready.send(Err("mpv could not decode or output the audio"));
                                }
                                Some(BackendEvent::Failed(
                                    "mpv could not decode or output the audio".into(),
                                ))
                            }
                            _ => None,
                        },
                        Some("property-change") => match value["name"].as_str() {
                            Some("time-pos") => value["data"]
                                .as_f64()
                                .filter(|v| v.is_finite() && *v >= 0.0)
                                .map(BackendEvent::Position),
                            Some("duration") => value["data"]
                                .as_f64()
                                .filter(|v| v.is_finite() && *v > 0.0)
                                .map(BackendEvent::Duration),
                            Some("pause") => value["data"].as_bool().map(BackendEvent::Pause),
                            Some("paused-for-cache") => {
                                value["data"].as_bool().map(BackendEvent::Buffering)
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(event) = event
                        && events.send(Event { generation, event }).await.is_err()
                    {
                        break;
                    }
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            // Pending commands fail immediately rather than waiting for their timeout.
            replies.lock().await.clear();
            let message = match result {
                Ok(()) => "mpv disconnected".into(),
                Err(_) => "Invalid mpv IPC stream".into(),
            };
            let _ = events
                .send(Event {
                    generation,
                    event: BackendEvent::Failed(message),
                })
                .await;
        });
        let mut backend = Self {
            child,
            writer,
            pending,
            next_id: AtomicU64::new(1),
            reader,
            request_timeout: Duration::from_secs(8),
        };
        for (id, name) in [
            (1, "time-pos"),
            (2, "duration"),
            (3, "pause"),
            (4, "paused-for-cache"),
        ] {
            backend
                .command(json!(["observe_property", id, name]))
                .await?;
            backend.request_timeout = Duration::from_secs(2);
        }
        let mut header_fields = Vec::new();
        for (name, value) in headers {
            reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .context("Invalid media header name")?;
            reqwest::header::HeaderValue::from_str(value).context("Invalid media header value")?;
            header_fields.push(format!("{name}: {value}"));
        }
        backend
            .command(json!(["set_property", "http-header-fields", header_fields]))
            .await?;
        backend
            .command(json!(["loadfile", path, "replace"]))
            .await?;
        timeout(
            Duration::from_secs(
                if path.starts_with("http://") || path.starts_with("https://") {
                    30
                } else {
                    4
                },
            ),
            ready_rx,
        )
        .await
        .context("mpv loading timed out")?
        .context("mpv exited during loading")?
        .map_err(anyhow::Error::msg)?;
        backend.pause(false).await?;
        Ok(backend)
    }

    async fn command(&mut self, command: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let mut frame = serde_json::to_vec(&json!({"command": command, "request_id": id}))?;
        frame.push(b'\n');
        let result = timeout(self.request_timeout, async {
            self.writer.write_all(&frame).await?;
            rx.await
                .context("mpv disconnected")?
                .map_err(anyhow::Error::msg)
        })
        .await;
        self.pending.lock().await.remove(&id);
        result.context("mpv command timed out")?
    }

    pub async fn pause(&mut self, paused: bool) -> Result<()> {
        self.command(json!(["set_property", "pause", paused]))
            .await?;
        Ok(())
    }
    pub async fn seek(&mut self, seconds: f64) -> Result<()> {
        self.command(json!(["seek", seconds, "absolute+exact"]))
            .await?;
        Ok(())
    }
    pub async fn volume(&mut self, percent: u8) -> Result<()> {
        self.command(json!(["set_property", "volume", percent]))
            .await?;
        Ok(())
    }

    pub async fn fade_out(mut self, seconds: f64, volume: u8) {
        let steps = (seconds * 20.0).ceil().max(1.0) as u32;
        for step in 1..=steps {
            tokio::time::sleep(Duration::from_secs_f64(seconds / steps as f64)).await;
            if self
                .volume((volume as f64 * (1.0 - step as f64 / steps as f64)).round() as u8)
                .await
                .is_err()
            {
                break;
            }
        }
        self.shutdown().await;
    }

    pub async fn shutdown(mut self) {
        self.reader.abort();
        let _ = self.child.kill().await;
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        self.reader.abort();
    }
}
