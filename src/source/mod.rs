mod crypto;
pub mod worker;
use crate::{
    domain::{ResolvedMedia, Track},
    paths::AppPaths,
    storage::{RecordKind, Storage},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::timeout,
};
use uuid::Uuid;

pub const SCRIPT_LIMIT: usize = 9_000_000;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capability {
    pub actions: Vec<String>,
    pub qualitys: Vec<String>,
    #[serde(rename = "type")]
    pub kind: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceRecord {
    pub id: Uuid,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub homepage: String,
    pub sha256: String,
    pub origin: String,
    pub script_path: PathBuf,
    pub capabilities: BTreeMap<String, Capability>,
}

impl SourceRecord {
    fn parse(script: &str, origin: &str, id: Uuid, paths: &AppPaths) -> Result<Self> {
        ensure!(
            !script.trim().is_empty() && script.len() <= SCRIPT_LIMIT,
            "Source script is empty or exceeds 9 MB"
        );
        let hash = Sha256::digest(script.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let prefix = script.chars().take(8192).collect::<String>();
        let trimmed = prefix.trim_start_matches('\u{feff}').trim_start();
        let comment = trimmed
            .strip_prefix("/*")
            .and_then(|text| text.split_once("*/").map(|(s, _)| s))
            .unwrap_or_default();
        let field = |key: &str| {
            let pattern = format!(r"(?m)@{key}[ \t]+([^\r\n]*)");
            regex::Regex::new(&pattern)
                .ok()
                .and_then(|re| {
                    re.captures(comment).map(|c| {
                        c[1].trim()
                            .trim_end_matches("*/")
                            .trim()
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(1024)
                            .collect::<String>()
                    })
                })
                .unwrap_or_default()
        };
        let mut name = field("name");
        if name.is_empty() {
            name = "Imported source".into();
        }
        Ok(Self {
            id,
            name,
            version: field("version"),
            author: field("author"),
            description: field("description"),
            homepage: field("homepage"),
            script_path: paths
                .data
                .join("sources")
                .join(id.to_string())
                .join(format!("{hash}.js")),
            sha256: hash,
            origin: origin.into(),
            capabilities: BTreeMap::new(),
        })
    }
}

pub struct SourceManager {
    storage: Storage,
    paths: AppPaths,
    client: reqwest::Client,
    active: Mutex<Option<(Uuid, Worker)>>,
}

impl SourceManager {
    pub fn new(paths: AppPaths, storage: Storage) -> Result<Self> {
        Ok(Self {
            paths,
            storage,
            client: crate::http::client()?,
            active: Mutex::new(None),
        })
    }
    pub async fn list(&self) -> Result<Vec<SourceRecord>> {
        self.storage
            .record_list(RecordKind::Source)
            .await?
            .into_iter()
            .map(|value| serde_json::from_value(value).map_err(Into::into))
            .collect()
    }
    pub async fn get(&self, id: Uuid) -> Result<SourceRecord> {
        serde_json::from_value(
            self.storage
                .record_get(RecordKind::Source, id.to_string())
                .await?
                .context("Source not found")?,
        )
        .map_err(Into::into)
    }
    pub async fn import(&self, location: String, replace: Option<Uuid>) -> Result<SourceRecord> {
        let bytes = if location.starts_with("http://") || location.starts_with("https://") {
            crate::http::bytes(
                self.client
                    .get(crate::http::url(&location)?)
                    .send()
                    .await
                    .map_err(|_| anyhow::anyhow!("Source download failed"))?,
                SCRIPT_LIMIT,
            )
            .await?
        } else {
            let path = PathBuf::from(&location);
            ensure!(
                tokio::fs::metadata(&path).await?.len() <= SCRIPT_LIMIT as u64,
                "Source exceeds 9 MB"
            );
            tokio::fs::read(path).await?
        };
        let script = String::from_utf8(bytes).context("Source script must be UTF-8")?;
        let mut record = SourceRecord::parse(
            &script,
            &location,
            replace.unwrap_or_else(Uuid::new_v4),
            &self.paths,
        )?;
        let write_path = record.script_path.clone();
        let code = script.into_bytes();
        tokio::task::spawn_blocking(move || crate::config::atomic_write(&write_path, &code))
            .await??;
        let (candidate, capabilities) = Worker::start(&record).await?;
        ensure!(
            !capabilities.is_empty(),
            "Source declared no supported platforms"
        );
        record.capabilities = capabilities;
        self.storage
            .record_put(
                RecordKind::Source,
                record.id.to_string(),
                serde_json::to_value(&record)?,
            )
            .await?;
        let mut active = self.active.lock().await;
        if active.as_ref().is_some_and(|(id, _)| *id == record.id) {
            *active = Some((record.id, candidate));
        }
        Ok(record)
    }
    pub async fn stage_inactive(&self, script: String, origin: String) -> Result<SourceRecord> {
        let record = SourceRecord::parse(&script, &origin, Uuid::new_v4(), &self.paths)?;
        let path = record.script_path.clone();
        tokio::task::spawn_blocking(move || crate::config::atomic_write(&path, script.as_bytes()))
            .await??;
        Ok(record)
    }
    pub async fn activate(&self, id: Uuid) -> Result<SourceRecord> {
        let mut record = self.get(id).await?;
        let (worker, capabilities) = Worker::start(&record).await?;
        record.capabilities = capabilities;
        self.storage
            .record_put(
                RecordKind::Source,
                id.to_string(),
                serde_json::to_value(&record)?,
            )
            .await?;
        *self.active.lock().await = Some((id, worker));
        Ok(record)
    }
    pub async fn stop(&self) {
        self.active.lock().await.take();
    }
    pub async fn remove(&self, id: Uuid) -> Result<()> {
        let mut active = self.active.lock().await;
        if active.as_ref().is_some_and(|(current, _)| *current == id) {
            active.take();
        }
        ensure!(
            self.storage
                .record_delete(RecordKind::Source, id.to_string())
                .await?,
            "Source not found"
        );
        Ok(())
    }
    pub async fn resolve(
        &self,
        selected: Uuid,
        track: &Track,
        quality: &str,
    ) -> Result<ResolvedMedia> {
        let platform = track
            .platform
            .context("Local files do not require a source")?
            .to_string();
        let record = self.get(selected).await?;
        let capability = record
            .capabilities
            .get(&platform)
            .context("Source does not support this platform")?;
        ensure!(
            capability.actions.iter().any(|a| a == "musicUrl")
                && capability.qualitys.iter().any(|q| q == quality),
            "Source does not support {platform} {quality}; select a source/quality explicitly"
        );
        let mut active = self.active.lock().await;
        if active.is_none() {
            let (worker, _) = Worker::start(&record).await?;
            *active = Some((selected, worker));
        }
        let (id, mut worker) = active.take().context("Source is not running")?;
        ensure!(id == selected, "Active source changed; retry");
        let response=worker.call(json!({"op":"resolve","source":platform,"action":"musicUrl","info":{"type":quality,"musicInfo":track.lx_info()}}),Duration::from_secs(65)).await?;
        let url = response["url"]
            .as_str()
            .context("Source returned no URL")?
            .to_string();
        ensure!(url.len() <= 8192, "Source URL exceeds limit");
        crate::http::url(&url)?;
        let headers = response["headers"]
            .as_object()
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        *active = Some((id, worker));
        Ok(ResolvedMedia {
            url,
            headers,
            expires_at: response["expires_at"].as_u64(),
        })
    }
}

struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}
impl Worker {
    async fn start(record: &SourceRecord) -> Result<(Self, BTreeMap<String, Capability>)> {
        let bytes = tokio::fs::read(&record.script_path).await?;
        ensure!(bytes.len() <= SCRIPT_LIMIT, "Source snapshot exceeds limit");
        let hash = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        ensure!(hash == record.sha256, "Source snapshot hash mismatch");
        let mut child = Command::new(std::env::current_exe()?)
            .arg("source-worker")
            .arg(&record.script_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut worker = Self {
            input: child.stdin.take().context("Worker has no input")?,
            output: BufReader::new(child.stdout.take().context("Worker has no output")?),
            child,
        };
        let result=worker.call(json!({"op":"init","meta":{"name":record.name,"version":record.version,"author":record.author,"description":record.description,"homepage":record.homepage}}),Duration::from_secs(15)).await?;
        let capabilities = serde_json::from_value(result["sources"].clone())
            .context("Invalid source capabilities")?;
        Ok((worker, capabilities))
    }
    async fn call(&mut self, mut command: Value, limit: Duration) -> Result<Value> {
        let id = Uuid::new_v4().to_string();
        command["id"] = json!(id);
        let mut bytes = serde_json::to_vec(&command)?;
        bytes.push(b'\n');
        ensure!(
            bytes.len() <= crate::protocol::MAX_FRAME,
            "Worker frame exceeds limit"
        );
        let result = timeout(limit, async {
            self.input.write_all(&bytes).await?;
            loop {
                let frame = crate::protocol::read_frame(&mut self.output)
                    .await?
                    .context("Source worker exited")?;
                let value: Value = serde_json::from_slice(&frame)?;
                if value["id"] != id {
                    continue;
                }
                if value["error"].is_string() {
                    anyhow::bail!(
                        "{}",
                        value["error"].as_str().unwrap_or("Source request failed")
                    );
                }
                return Ok(value["result"].clone());
            }
        })
        .await;
        match result {
            Ok(value) => value,
            Err(_) => {
                let _ = self.input.shutdown().await;
                if timeout(Duration::from_millis(500), self.child.wait())
                    .await
                    .is_err()
                {
                    let _ = self.child.start_kill();
                    let _ = self.child.wait().await;
                }
                anyhow::bail!("Source request timed out; worker stopped")
            }
        }
    }
}
