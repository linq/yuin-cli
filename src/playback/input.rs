//! Bounded disk spool; network waits happen only on the decoder thread.
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use symphonia::core::io::MediaSource;
use tokio_util::sync::CancellationToken;
const MAX_BYTES: u64 = 512 * 1024 * 1024;
#[derive(Default)]
struct Progress {
    bytes: u64,
    done: bool,
    error: Option<String>,
}
struct Spool {
    file: File,
    progress: Mutex<Progress>,
    changed: Condvar,
    length: Option<u64>,
    cancel: CancellationToken,
}
struct Reader {
    spool: Arc<Spool>,
    position: u64,
}

pub struct Input {
    pub source: Box<dyn MediaSource>,
    pub progressive: bool,
}

pub async fn open(
    path: &str,
    headers: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Input> {
    if !path.starts_with("http://") && !path.starts_with("https://") {
        return Ok(Input {
            source: Box::new(File::open(path).context("Cannot open audio file")?),
            progressive: false,
        });
    }
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 Yuin-CLI/0.1")
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;
    let mut request = client.get(crate::http::url(path)?);
    for (name, value) in headers {
        request = request.header(
            reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .context("Invalid media header name")?,
            reqwest::header::HeaderValue::from_str(value).context("Invalid media header value")?,
        );
    }
    let mut response = request
        .send()
        .await
        .context("Cannot fetch audio stream")?
        .error_for_status()
        .context("Audio HTTP request failed")?;
    let length = response.content_length();
    ensure!(
        length.is_none_or(|length| length <= MAX_BYTES),
        "Audio stream exceeds 512 MiB"
    );
    let mut writer = tempfile::tempfile()?;
    let spool = Arc::new(Spool {
        file: writer.try_clone()?,
        progress: Mutex::new(Progress::default()),
        changed: Condvar::new(),
        length,
        cancel: cancel.clone(),
    });
    let downloading = spool.clone();
    tokio::spawn(async move {
        let result = tokio::select! {
            _ = cancel.cancelled() => Err(anyhow::anyhow!("Audio loading cancelled")),
            result = tokio::time::timeout(Duration::from_secs(900), async {
                while let Some(chunk) = response.chunk().await? {
                    let bytes = downloading.progress.lock().unwrap().bytes;
                    ensure!(bytes + chunk.len() as u64 <= MAX_BYTES, "Audio stream exceeds 512 MiB");
                    writer.write_all(&chunk)?;
                    downloading.progress.lock().unwrap().bytes += chunk.len() as u64;
                    downloading.changed.notify_all();
                }
                ensure!(length.is_none_or(|length| length == downloading.progress.lock().unwrap().bytes), "Audio HTTP response is truncated");
                Ok::<_, anyhow::Error>(())
            }) => result.context("Audio download exceeded 15 minutes").and_then(|result| result),
        };
        let mut progress = downloading.progress.lock().unwrap();
        progress.done = true;
        progress.error = result.err().map(|error| format!("{error:#}"));
        downloading.changed.notify_all();
    });
    Ok(Input {
        source: Box::new(Reader { spool, position: 0 }),
        progressive: true,
    })
}
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut progress = self.spool.progress.lock().unwrap();
        while self.position >= progress.bytes && !progress.done {
            if self.spool.cancel.is_cancelled() {
                return Err(io::Error::other("Audio loading cancelled"));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Audio stream stalled",
                ));
            }
            progress = self
                .spool
                .changed
                .wait_timeout(progress, Duration::from_millis(100))
                .unwrap()
                .0;
        }
        if self.position >= progress.bytes {
            if let Some(error) = &progress.error {
                return Err(io::Error::other(error.clone()));
            }
            return Ok(0);
        }
        let available = (progress.bytes - self.position).min(bytes.len() as u64) as usize;
        drop(progress);
        let count = self
            .spool
            .file
            .read_at(&mut bytes[..available], self.position)?;
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for Reader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(position) => position as i128,
            SeekFrom::Current(offset) => self.position as i128 + offset as i128,
            SeekFrom::End(offset) => {
                // Some containers (notably MP4) need the end offset during probing.
                // Without Content-Length, wait for completion with the same bounded,
                // cancellable wait used by reads instead of claiming a false length.
                let deadline = Instant::now() + Duration::from_secs(20);
                let mut progress = self.spool.progress.lock().unwrap();
                while self.spool.length.is_none() && !progress.done {
                    if self.spool.cancel.is_cancelled() || Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "Audio length is unavailable",
                        ));
                    }
                    progress = self
                        .spool
                        .changed
                        .wait_timeout(progress, Duration::from_millis(100))
                        .unwrap()
                        .0;
                }
                if let Some(error) = &progress.error {
                    return Err(io::Error::other(error.clone()));
                }
                self.spool.length.unwrap_or(progress.bytes) as i128 + offset as i128
            }
        };
        if !(0..=MAX_BYTES as i128).contains(&position) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Audio seek exceeds stream limits",
            ));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}
impl MediaSource for Reader {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        self.spool.length.or_else(|| {
            let progress = self.spool.progress.lock().unwrap();
            progress.done.then_some(progress.bytes)
        })
    }
}
