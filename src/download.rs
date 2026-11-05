use crate::{domain::Track, media::MediaReport, services::Services, storage::RecordKind};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex, Semaphore},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Queued,
    Resolving,
    Downloading,
    Validating,
    Completed,
    Failed,
    Cancelled,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub id: Uuid,
    pub track: Track,
    pub quality: String,
    pub state: DownloadState,
    pub bytes: u64,
    pub total: Option<u64>,
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub directory: PathBuf,
    #[serde(default)]
    pub partial_path: Option<PathBuf>,
    pub report: Option<MediaReport>,
    pub error: Option<String>,
}
struct Task {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}
pub struct DownloadManager {
    tasks: Mutex<HashMap<Uuid, Task>>,
    limit: Semaphore,
}
impl DownloadManager {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            limit: Semaphore::new(2),
        }
    }
    pub async fn restore(&self, services: &Services) -> Result<()> {
        for value in services.storage.record_list(RecordKind::Download).await? {
            let mut record: DownloadRecord = serde_json::from_value(value)?;
            let mut changed = false;
            if let Some(part) = record.partial_path.take() {
                let expected = format!(".yuin-{}.part", record.id);
                if part.is_absolute()
                    && part
                        .file_name()
                        .is_some_and(|name| name == expected.as_str())
                {
                    let _ = tokio::fs::remove_file(part).await;
                }
                changed = true;
            }
            if matches!(
                record.state,
                DownloadState::Queued
                    | DownloadState::Resolving
                    | DownloadState::Downloading
                    | DownloadState::Validating
            ) {
                record.state = DownloadState::Cancelled;
                record.error = Some("Daemon stopped before completion; retry explicitly".into());
                changed = true;
            }
            if changed {
                save(services, &record).await?;
            }
        }
        Ok(())
    }
    pub async fn start(
        self: &Arc<Self>,
        services: Arc<Services>,
        track: Track,
        quality: String,
    ) -> Result<DownloadRecord> {
        ensure!(
            crate::config::QUALITIES.contains(&quality.as_str()),
            "Unsupported download quality"
        );
        let mut tasks = self.tasks.lock().await;
        tasks.retain(|_, task| !task.handle.is_finished());
        ensure!(
            tasks.len() < 100,
            "At most 100 pending download tasks are supported"
        );
        let record = DownloadRecord {
            id: Uuid::new_v4(),
            track,
            quality,
            state: DownloadState::Queued,
            bytes: 0,
            total: None,
            path: None,
            directory: services.config.read().await.download_dir.clone(),
            partial_path: None,
            report: None,
            error: None,
        };
        save(&services, &record).await?;
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let manager = self.clone();
        let initial = record.clone();
        let handle = tokio::spawn(async move {
            let mut record = initial;
            let result = manager.run(&services, &mut record, &task_cancel).await;
            if let Err(error) = result {
                record.state = if task_cancel.is_cancelled() {
                    DownloadState::Cancelled
                } else {
                    DownloadState::Failed
                };
                record.error = Some(redact_error(&error.to_string()));
            }
            record.partial_path = None;
            let _ = save(&services, &record).await;
        });
        tasks.insert(record.id, Task { cancel, handle });
        Ok(record)
    }
    async fn run(
        &self,
        services: &Services,
        record: &mut DownloadRecord,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let permits = if services.config.read().await.download_concurrency == 1 {
            2
        } else {
            1
        };
        let _permit = tokio::select! { biased;_ =cancel.cancelled()=>anyhow::bail!("Download cancelled"),permit=self.limit.acquire_many(permits)=>permit?};
        ensure!(!cancel.is_cancelled(), "Download cancelled");
        record.state = DownloadState::Resolving;
        save(services, record).await?;
        let media = tokio::select! { biased;_ =cancel.cancelled()=>anyhow::bail!("Download cancelled"),media=services.resolve_online(&record.track,Some(&record.quality))=>media?};
        let directory = if record.directory.as_os_str().is_empty() {
            services.config.read().await.download_dir.clone()
        } else {
            record.directory.clone()
        };
        tokio::fs::create_dir_all(&directory).await?;
        let part = directory.join(format!(".yuin-{}.part", record.id));
        record.partial_path = Some(part.clone());
        save(services, record).await?;
        let _cleanup = PartialFile(part.clone());
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
            .await?;
        let mut request = crate::http::client()?
            .get(crate::http::url(&media.url)?)
            .timeout(Duration::from_secs(1800));
        for (key, value) in media.headers {
            request = request.header(&key, &value);
        }
        let mut response = tokio::select! { biased;_ =cancel.cancelled()=>anyhow::bail!("Download cancelled"),response=request.send()=>response.map_err(|_|anyhow::anyhow!("Audio download network request failed"))?};
        ensure!(
            response.status().is_success(),
            "Audio download returned HTTP {}",
            response.status().as_u16()
        );
        let max = 512_u64 * 1024 * 1024;
        ensure!(
            response.content_length().is_none_or(|n| n <= max),
            "Audio exceeds 512 MiB"
        );
        record.total = response.content_length();
        record.state = DownloadState::Downloading;
        save(services, record).await?;
        let mut checkpoint = tokio::time::Instant::now();
        while let Some(chunk) = tokio::select! { biased;_ =cancel.cancelled()=>anyhow::bail!("Download cancelled"),chunk=response.chunk()=>chunk.map_err(|_|anyhow::anyhow!("Audio download interrupted"))?}
        {
            ensure!(!cancel.is_cancelled(), "Download cancelled");
            record.bytes += chunk.len() as u64;
            ensure!(record.bytes <= max, "Audio exceeds 512 MiB");
            file.write_all(&chunk).await?;
            if checkpoint.elapsed() > Duration::from_millis(250) {
                save(services, record).await?;
                checkpoint = tokio::time::Instant::now();
            }
        }
        ensure!(
            record.total.is_none_or(|size| record.bytes == size),
            "Audio download is truncated"
        );
        file.sync_all().await?;
        drop(file);
        record.state = DownloadState::Validating;
        save(services, record).await?;
        let validate_path = part.clone();
        let quality = record.quality.clone();
        let cancelled = cancel.clone();
        let report = tokio::task::spawn_blocking(move || {
            crate::media::validate(&validate_path, &quality, &cancelled)
        })
        .await??;
        ensure!(!cancel.is_cancelled(), "Download cancelled");
        use sha2::{Digest, Sha256};
        let identity = Sha256::digest(record.track.id.as_bytes())
            .iter()
            .take(12)
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let name = record
            .track
            .title
            .chars()
            .filter(|c| !c.is_control() && !"/\\:*?\"<>|".contains(*c))
            .take(80)
            .collect::<String>();
        let target = directory.join(format!(
            "{name} [{identity}-{}].{}",
            record.quality, report.extension
        ));
        // A hard link publishes a complete file atomically and never overwrites an existing target.
        tokio::fs::hard_link(&part, &target)
            .await
            .context("Cannot publish audio file; target may already exist")?;
        record.path = Some(target.clone());
        record.report = Some(report);
        record.state = DownloadState::Completed;
        if let Err(error) = save(services, record).await {
            let _ = tokio::fs::remove_file(target).await;
            record.path = None;
            record.report = None;
            return Err(error);
        }
        Ok(())
    }
    pub async fn cancel(&self, id: Uuid) -> Result<()> {
        let tasks = self.tasks.lock().await;
        let task = tasks.get(&id).context("Running download not found")?;
        ensure!(!task.handle.is_finished(), "Download has already finished");
        task.cancel.cancel();
        Ok(())
    }
    pub async fn shutdown(&self) {
        let mut tasks = self.tasks.lock().await;
        for task in tasks.values() {
            task.cancel.cancel();
        }
        for (_, task) in tasks.drain() {
            let _ = task.handle.await;
        }
    }
}
struct PartialFile(PathBuf);
impl Drop for PartialFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
async fn save(services: &Services, record: &DownloadRecord) -> Result<()> {
    services
        .storage
        .record_put(
            RecordKind::Download,
            record.id.to_string(),
            serde_json::to_value(record)?,
        )
        .await
}
fn redact_error(value: &str) -> String {
    regex::Regex::new(r#"https?://[^\s"<>]+"#)
        .map(|r| r.replace_all(value, "[URL]").to_string())
        .unwrap_or_else(|_| "Download failed".into())
}
