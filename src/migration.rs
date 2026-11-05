use crate::{
    domain::{LocalPlaylist, Track},
    download::{DownloadRecord, DownloadState},
    services::Services,
    storage::RecordKind,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub struct ImportPlan {
    lists: Vec<LocalPlaylist>,
    sources: Vec<(String, String)>,
    downloads: Vec<DownloadRecord>,
    history: Vec<Track>,
    missing_downloads: usize,
    queue_omitted: usize,
}
impl ImportPlan {
    pub fn summary(&self) -> Value {
        json!({"playlists":self.lists.len(),"tracks":self.lists.iter().map(|p|p.tracks.len()).sum::<usize>(),"sources":self.sources.len(),"verified_downloads":self.downloads.len(),"unavailable_downloads":self.missing_downloads,"history":self.history.len(),"queue_entries_not_imported":self.queue_omitted,"sources_enabled":false})
    }
    pub async fn commit(self, services: &Services) -> Result<Value> {
        let backup = services.storage.backup().await?;
        let summary = self.summary();
        let mut sources = Vec::new();
        for (script, origin) in self.sources {
            sources.push(serde_json::to_value(
                services.sources.stage_inactive(script, origin).await?,
            )?);
        }
        services
            .storage
            .import_records(
                self.lists,
                sources,
                self.downloads
                    .into_iter()
                    .map(serde_json::to_value)
                    .collect::<Result<_, _>>()?,
                self.history,
            )
            .await?;
        Ok(json!({"imported":summary,"backup":backup}))
    }
}

fn track(value: Value) -> Result<Track> {
    let mut track: Track = serde_json::from_value(value)?;
    if let Some(platform) = track.platform {
        ensure!(!track.song_id.is_empty(), "Imported track has no song ID");
        track.id = format!("{platform}:{}", track.song_id);
    } else {
        ensure!(
            track.id.starts_with("local:") && Path::new(&track.path).is_absolute(),
            "Invalid local track identity"
        );
    }
    ensure!(
        !track.title.trim().is_empty()
            && track.title.len() <= 4096
            && track.source_info.is_object()
            && track.duration.is_finite()
            && track.duration >= 0.0,
        "Invalid track metadata"
    );
    Ok(track)
}

pub async fn preview(path: PathBuf) -> Result<ImportPlan> {
    let metadata = tokio::fs::metadata(&path).await?;
    ensure!(metadata.len() <= 32_000_000, "Import exceeds 32 MB");
    let data: Value = serde_json::from_slice(&tokio::fs::read(&path).await?)?;
    let desktop = data["schema"] == 1;
    let native = data["schema_version"] == 1 && data["format"] == "yuin-cli";
    ensure!(desktop || native, "Unsupported library export schema");
    let rows = data["playlists"]
        .as_array()
        .context("Export has no playlists array")?;
    ensure!(rows.len() <= 1000, "Too many personal playlists");
    let mut lists = Vec::new();
    let mut total = 0;
    for row in rows {
        let name = row["name"]
            .as_str()
            .context("Playlist has no name")?
            .to_string();
        ensure!(
            !name.trim().is_empty() && name.len() <= 256,
            "Invalid playlist name"
        );
        let tracks = row["tracks"]
            .as_array()
            .context("Playlist has no tracks")?
            .iter()
            .cloned()
            .map(track)
            .collect::<Result<Vec<_>>>()?;
        total += tracks.len();
        ensure!(
            tracks.len() <= 10000 && total <= 50000,
            "Import track limit exceeded"
        );
        lists.push(LocalPlaylist {
            id: Uuid::new_v4(),
            name,
            tracks,
        });
    }
    let mut sources = Vec::new();
    for row in data["sources"].as_array().into_iter().flatten() {
        let script = row["script"]
            .as_str()
            .context("Exported source has no embedded snapshot")?
            .to_string();
        ensure!(
            !script.trim().is_empty() && script.len() <= crate::source::SCRIPT_LIMIT,
            "Invalid source snapshot"
        );
        sources.push((
            script,
            row["origin"].as_str().unwrap_or("imported library").into(),
        ));
    }
    ensure!(sources.len() <= 100, "Too many sources");
    let mut downloads = Vec::new();
    let mut missing_downloads = 0;
    for row in data["downloads"].as_array().into_iter().flatten() {
        let finished = if desktop {
            row["status"] == "completed"
        } else {
            row["state"] == "completed"
        };
        if !finished {
            continue;
        }
        let file = if desktop {
            row["file"].as_str()
        } else {
            row["path"].as_str()
        };
        let Some(path) = file
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_file())
        else {
            missing_downloads += 1;
            continue;
        };
        let quality = row["quality"]
            .as_str()
            .context("Download has no quality")?
            .to_string();
        ensure!(
            crate::config::QUALITIES.contains(&quality.as_str()),
            "Unknown imported download quality"
        );
        let check_path = path.clone();
        let check_quality = quality.clone();
        let report = tokio::task::spawn_blocking(move || {
            crate::media::validate(
                &check_path,
                &check_quality,
                &tokio_util::sync::CancellationToken::new(),
            )
        })
        .await?;
        match report {
            Ok(report) => downloads.push(DownloadRecord {
                id: Uuid::new_v4(),
                track: track(row["track"].clone())?,
                quality,
                state: DownloadState::Completed,
                bytes: report.bytes,
                total: Some(report.bytes),
                directory: path.parent().unwrap_or(Path::new("/")).to_path_buf(),
                partial_path: None,
                path: Some(path),
                report: Some(report),
                error: None,
            }),
            Err(_) => missing_downloads += 1,
        }
    }
    let history = data["history"]
        .as_array()
        .into_iter()
        .flatten()
        .take(1000)
        .cloned()
        .map(|value| {
            track(if value["track"].is_object() {
                value["track"].clone()
            } else {
                value
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let queue_omitted = data["queue"]["entries"]
        .as_array()
        .or(data["queue"]["queue"].as_array())
        .map_or(0, Vec::len);
    Ok(ImportPlan {
        lists,
        sources,
        downloads,
        history,
        missing_downloads,
        queue_omitted,
    })
}

pub async fn export(services: &Services, path: PathBuf, include_sources: bool) -> Result<Value> {
    ensure!(!path.exists(), "Export destination already exists");
    let mut sources = Vec::new();
    if include_sources {
        for record in services.sources.list().await? {
            let script = tokio::fs::read_to_string(&record.script_path).await?;
            sources.push(json!({"name":record.name,"version":record.version,"origin":record.origin,"script":script,"sha256":record.sha256}));
        }
    }
    let value = json!({"schema_version":1,"format":"yuin-cli","playlists":services.storage.record_list(RecordKind::Playlist).await?,"sources":sources,"downloads":services.storage.record_list(RecordKind::Download).await?,"history":services.storage.history().await?});
    let bytes = serde_json::to_vec_pretty(&value)?;
    ensure!(bytes.len() <= 32_000_000, "Export exceeds 32 MB");
    let size = bytes.len();
    let destination = path.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        use std::io::Write;
        let parent = destination.parent().context("Export path needs a parent")?;
        ensure!(parent.is_dir(), "Export parent directory does not exist");
        let temp = parent.join(format!(".yuin-export-{}", Uuid::new_v4()));
        let result = (|| {
            let mut file = crate::secure::private_file(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::hard_link(&temp, &destination)?;
            Ok(())
        })();
        let _ = std::fs::remove_file(temp);
        result
    })
    .await??;
    Ok(json!({"path":path,"bytes":size,"includes_sources":include_sources}))
}
