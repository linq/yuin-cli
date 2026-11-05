use crate::{
    catalog::Catalog,
    config::Config,
    domain::{Platform, ResolvedMedia, Track},
    paths::AppPaths,
    protocol::RpcError,
    source::SourceManager,
    storage::{LibraryEdit, RecordKind, Storage},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceCommand {
    Search {
        query: String,
        platform: Option<Platform>,
        page: u32,
    },
    SearchCollections {
        query: String,
        platform: Option<Platform>,
        page: u32,
    },
    Collections {
        platform: Option<Platform>,
        charts: bool,
        page: u32,
    },
    CollectionTracks {
        id: String,
        chart: bool,
        page: u32,
        all: bool,
    },
    Track {
        id: String,
    },
    SourceImport {
        location: String,
    },
    SourceList,
    SourceUse {
        id: Uuid,
    },
    SourceUpdate {
        id: Uuid,
    },
    SourceRemove {
        id: Uuid,
    },
    SourceStop,
    ConfigGet,
    ConfigSet {
        key: String,
        value: String,
    },
    Favorite {
        track: String,
    },
    LibraryList,
    LibraryCreate {
        name: String,
    },
    LibraryShow {
        id: Uuid,
        #[serde(default = "first_page")]
        page: u32,
    },
    LibraryAdd {
        id: Uuid,
        track: String,
    },
    LibrarySave {
        name: String,
        collection: String,
        chart: bool,
    },
    LibraryFavorite {
        collection: String,
        name: String,
        chart: bool,
    },
    LibraryRename {
        id: Uuid,
        name: String,
    },
    LibraryRemove {
        id: Uuid,
        index: usize,
    },
    LibraryMove {
        id: Uuid,
        from: usize,
        to: usize,
    },
    LibraryDelete {
        id: Uuid,
    },
    Lyrics {
        track: String,
        refresh: bool,
    },
    Import {
        path: String,
        commit: bool,
    },
    Export {
        path: String,
        include_sources: bool,
    },
    History {
        #[serde(default = "first_page")]
        page: u32,
    },
    Download {
        track: String,
        quality: Option<String>,
    },
    Downloads {
        #[serde(default = "first_page")]
        page: u32,
    },
    DownloadShow {
        id: Uuid,
    },
    DownloadCancel {
        id: Uuid,
    },
    DownloadRetry {
        id: Uuid,
    },
}

pub struct Services {
    pub paths: AppPaths,
    pub storage: Storage,
    pub config: Arc<RwLock<Config>>,
    pub sources: Arc<SourceManager>,
    pub catalog: Catalog,
    pub lyrics: crate::lyrics::LyricsService,
    pub downloads: Arc<crate::download::DownloadManager>,
}
impl Services {
    pub async fn new(paths: AppPaths, storage: Storage) -> Result<Self> {
        let p = paths.clone();
        let config = tokio::task::spawn_blocking(move || Config::load(&p)).await??;
        let services = Self {
            downloads: Arc::new(crate::download::DownloadManager::new()),
            lyrics: crate::lyrics::LyricsService::new(&paths)?,
            sources: Arc::new(SourceManager::new(paths.clone(), storage.clone())?),
            catalog: Catalog::new()?,
            paths,
            storage,
            config: Arc::new(RwLock::new(config)),
        };
        services.downloads.restore(&services).await?;
        Ok(services)
    }
    pub async fn collection_tracks(&self, id: &str, chart: bool) -> Result<Vec<Track>> {
        self.catalog.all_tracks(id, chart).await.map_err(network)
    }

    pub async fn library_tracks(&self, id: Uuid) -> Result<Vec<Track>> {
        let value = self
            .storage
            .record_get(RecordKind::Playlist, id.to_string())
            .await?
            .ok_or_else(|| RpcError::new(6, "Library list not found"))?;
        Ok(serde_json::from_value::<crate::domain::LocalPlaylist>(value)?.tracks)
    }

    pub async fn track(&self, input: &str) -> Result<Track> {
        if input.starts_with("local:")
            && let Some(track) = self.storage.track(input.to_string()).await?
        {
            return Ok(track);
        }
        if crate::catalog::parse_id(input).is_ok() {
            if let Some(track) = self.storage.track(input.to_string()).await? {
                return Ok(track);
            }
            let track = self
                .catalog
                .detail(input)
                .await
                .map_err(|e| RpcError::new(3, e.to_string()))?;
            self.storage.put_tracks(vec![track.clone()]).await?;
            Ok(track)
        } else {
            let path = input.to_string();
            tokio::task::spawn_blocking(move || Track::local(std::path::Path::new(&path)))
                .await?
                .map_err(|e| RpcError::new(6, e.to_string()).into())
        }
    }
    pub async fn resolve(&self, track: &Track, quality: Option<&str>) -> Result<ResolvedMedia> {
        let requested = quality
            .map(str::to_string)
            .unwrap_or(self.config.read().await.play_quality.clone());
        for value in self
            .storage
            .offline_records(track.id.clone(), requested.clone())
            .await?
        {
            let record: crate::download::DownloadRecord = serde_json::from_value(value)?;
            if record.state == crate::download::DownloadState::Completed
                && record.track.id == track.id
                && record.quality == requested
                && let (Some(path), Some(report)) = (record.path, record.report)
            {
                let check_path = path.clone();
                let q = requested.clone();
                let validated = tokio::task::spawn_blocking(move || {
                    crate::media::validate(
                        &check_path,
                        &q,
                        &tokio_util::sync::CancellationToken::new(),
                    )
                })
                .await?;
                if let Ok(current) = validated
                    && current.sha256 == report.sha256
                {
                    return Ok(ResolvedMedia {
                        url: path.to_string_lossy().into_owned(),
                        headers: Default::default(),
                        expires_at: None,
                    });
                }
            }
        }
        self.resolve_online(track, Some(&requested)).await
    }
    pub async fn resolve_online(
        &self,
        track: &Track,
        quality: Option<&str>,
    ) -> Result<ResolvedMedia> {
        if track.platform.is_none() {
            ensure!(
                std::path::Path::new(&track.path).is_file(),
                "Local audio file is missing"
            );
            return Ok(ResolvedMedia {
                url: track.path.clone(),
                headers: Default::default(),
                expires_at: None,
            });
        }
        let config = self.config.read().await.clone();
        let quality = quality.unwrap_or(&config.play_quality);
        let source = config.source.ok_or_else(|| {
            RpcError::new(
                4,
                "No music source is enabled; import and select a trusted LX source",
            )
        })?;
        self.sources
            .resolve(source, track, quality)
            .await
            .map_err(|e| RpcError::new(4, e.to_string()).into())
    }
    async fn persist_config(&self, config: &Config) -> Result<()> {
        let path = self.paths.config.clone();
        let bytes = toml::to_string_pretty(config)?.into_bytes();
        tokio::task::spawn_blocking(move || crate::config::atomic_write(&path, &bytes)).await?
    }
    pub async fn execute(self: &Arc<Self>, command: ServiceCommand) -> Result<Value> {
        use ServiceCommand::*;
        let value = match command {
            Search {
                query,
                platform,
                page,
            } => {
                let platform = platform.unwrap_or(self.config.read().await.platform);
                let result = self
                    .catalog
                    .search(&query, platform, page)
                    .await
                    .map_err(network)?;
                self.storage.put_tracks(result.items.clone()).await?;
                serde_json::to_value(result)?
            }
            SearchCollections {
                query,
                platform,
                page,
            } => {
                let platform = platform.unwrap_or(self.config.read().await.platform);
                serde_json::to_value(
                    self.catalog
                        .search_collections(&query, platform, page)
                        .await
                        .map_err(network)?,
                )?
            }
            Collections {
                platform,
                charts,
                page,
            } => {
                let platform = platform.unwrap_or(self.config.read().await.platform);
                serde_json::to_value(
                    self.catalog
                        .collections(platform, charts, page)
                        .await
                        .map_err(network)?,
                )?
            }
            CollectionTracks {
                id,
                chart,
                page,
                all,
            } => {
                if all {
                    let tracks = self.catalog.all_tracks(&id, chart).await.map_err(network)?;
                    self.storage.put_tracks(tracks.clone()).await?;
                    json!({"items":tracks,"total":tracks.len(),"has_more":false,"page":1})
                } else {
                    let response = self
                        .catalog
                        .tracks(&id, chart, page)
                        .await
                        .map_err(network)?;
                    self.storage.put_tracks(response.items.clone()).await?;
                    serde_json::to_value(response)?
                }
            }
            Track { id } => json!({"track":self.track(&id).await?}),
            SourceImport { location } => {
                json!({"source":self.sources.import(location,None).await.map_err(source)?})
            }
            SourceList => {
                json!({"items":self.sources.list().await?,"active":self.config.read().await.source})
            }
            SourceUse { id } => {
                let mut config = self.config.write().await;
                let record = self.sources.activate(id).await.map_err(source)?;
                let mut candidate = config.clone();
                candidate.source = Some(id);
                if let Err(error) = self.persist_config(&candidate).await {
                    self.sources.stop().await;
                    return Err(error);
                }
                *config = candidate;
                json!({"source":record,"active":id})
            }
            SourceUpdate { id } => {
                let record = self.sources.get(id).await.map_err(source)?;
                json!({"source":self.sources.import(record.origin,Some(id)).await.map_err(source)?})
            }
            SourceRemove { id } => {
                let mut config = self.config.write().await;
                if config.source == Some(id) {
                    let mut candidate = config.clone();
                    candidate.source = None;
                    self.persist_config(&candidate).await?;
                    *config = candidate;
                }
                self.sources.remove(id).await.map_err(source)?;
                json!({"removed":id})
            }
            SourceStop => {
                let mut config = self.config.write().await;
                let mut candidate = config.clone();
                candidate.source = None;
                self.persist_config(&candidate).await?;
                *config = candidate;
                self.sources.stop().await;
                json!({"active":null})
            }
            ConfigGet => json!({"config":self.config.read().await.clone()}),
            ConfigSet { key, value } => {
                let mut config = self.config.write().await;
                let mut candidate = config.clone();
                candidate
                    .set(&key, &value)
                    .map_err(|e| RpcError::new(2, e.to_string()))?;
                self.persist_config(&candidate).await?;
                *config = candidate;
                json!({"config":config.clone(),"restart_required":key=="media_keys"})
            }
            Favorite { track } => {
                let track = self.track(&track).await?;
                let id = Uuid::nil();
                if self
                    .storage
                    .record_get(RecordKind::Playlist, id.to_string())
                    .await?
                    .is_none()
                {
                    let name = if self.config.read().await.language == "en" {
                        "Favorites"
                    } else {
                        "我喜欢的音乐"
                    }
                    .into();
                    self.storage
                        .library_edit(id, LibraryEdit::Create { name })
                        .await?;
                }
                json!({"playlist":self.storage.library_edit(id,LibraryEdit::Add{tracks:vec![track]}).await?})
            }
            LibraryList => {
                let values = self.storage.record_list(RecordKind::Playlist).await?;
                let items=values.into_iter().map(|v|json!({"id":v["id"],"name":v["name"],"count":v["tracks"].as_array().map_or(0,Vec::len)})).collect::<Vec<_>>();
                json!({"items":items})
            }
            LibraryCreate { name } => {
                validate_name(&name)?;
                json!({"playlist":self.storage.library_edit(Uuid::new_v4(),LibraryEdit::Create{name}).await?})
            }
            LibraryShow { id, page } => {
                let mut list = self
                    .storage
                    .record_get(RecordKind::Playlist, id.to_string())
                    .await?
                    .ok_or_else(|| RpcError::new(6, "Personal list not found"))?;
                let tracks = list["tracks"].as_array().cloned().unwrap_or_default();
                let mut result = paginate(tracks, page, 100)?;
                list["tracks"] = result["items"].clone();
                result["playlist"] = list;
                result
            }
            LibraryAdd { id, track } => {
                let track = self.track(&track).await?;
                json!({"playlist":self.storage.library_edit(id,LibraryEdit::Add{tracks:vec![track]}).await?})
            }
            LibraryFavorite {
                collection,
                name,
                chart,
            } => {
                validate_name(&name)?;
                crate::catalog::parse_id(&collection)?;
                let id = Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("yuin:collection:{chart}:{collection}").as_bytes(),
                );
                if let Some(value) = self
                    .storage
                    .record_get(RecordKind::Playlist, id.to_string())
                    .await?
                {
                    let list: crate::domain::LocalPlaylist = serde_json::from_value(value)?;
                    json!({"playlist":{"id":list.id,"name":list.name,"count":list.tracks.len()},"already_saved":true})
                } else {
                    let tracks = self.collection_tracks(&collection, chart).await?;
                    let list = self
                        .storage
                        .library_edit(id, LibraryEdit::CreateOnce { name, tracks })
                        .await?;
                    json!({"playlist":{"id":list.id,"name":list.name,"count":list.tracks.len()},"already_saved":false})
                }
            }
            LibrarySave {
                name,
                collection,
                chart,
            } => {
                validate_name(&name)?;
                let tracks = self
                    .catalog
                    .all_tracks(&collection, chart)
                    .await
                    .map_err(network)?;
                let id = Uuid::new_v4();
                json!({"playlist":self.storage.library_edit(id,LibraryEdit::CreateWithTracks{name,tracks}).await?})
            }
            LibraryRename { id, name } => {
                validate_name(&name)?;
                json!({"playlist":self.storage.library_edit(id,LibraryEdit::Rename{name}).await?})
            }
            LibraryRemove { id, index } => {
                json!({"playlist":self.storage.library_edit(id,LibraryEdit::Remove{index}).await?})
            }
            LibraryMove { id, from, to } => {
                json!({"playlist":self.storage.library_edit(id,LibraryEdit::Move{from,to}).await?})
            }
            LibraryDelete { id } => {
                ensure!(
                    self.storage
                        .record_delete(RecordKind::Playlist, id.to_string())
                        .await?,
                    "Personal list not found"
                );
                json!({"removed":id})
            }
            Download { track, quality } => {
                let track = self.track(&track).await?;
                ensure!(
                    track.platform.is_some(),
                    "Download accepts a platform track ID; local files are already offline"
                );
                let quality = quality.unwrap_or(self.config.read().await.download_quality.clone());
                json!({"download":self.downloads.start(self.clone(),track,quality).await?})
            }
            Downloads { page } => paginate(
                self.storage.record_list(RecordKind::Download).await?,
                page,
                100,
            )?,
            DownloadShow { id } => {
                json!({"download":self.storage.record_get(RecordKind::Download,id.to_string()).await?.ok_or_else(||RpcError::new(6,"Download not found"))?})
            }
            DownloadCancel { id } => {
                self.downloads.cancel(id).await?;
                json!({"cancelled":id})
            }
            DownloadRetry { id } => {
                let previous = self
                    .storage
                    .record_get(RecordKind::Download, id.to_string())
                    .await?
                    .context("Download not found")?;
                let record: crate::download::DownloadRecord = serde_json::from_value(previous)?;
                ensure!(
                    matches!(
                        record.state,
                        crate::download::DownloadState::Failed
                            | crate::download::DownloadState::Cancelled
                    ),
                    "Only failed or cancelled tasks can be retried"
                );
                json!({"download":self.downloads.start(self.clone(),record.track,record.quality).await?})
            }
            Lyrics { track, refresh } => {
                let track = self.track(&track).await?;
                json!({"lyrics":self.lyrics.get(&track,refresh).await.map_err(network)?})
            }
            Import { path, commit } => {
                let plan = crate::migration::preview(path.into()).await?;
                if commit {
                    plan.commit(self).await?
                } else {
                    json!({"preview":plan.summary(),"write_performed":false})
                }
            }
            Export {
                path,
                include_sources,
            } => crate::migration::export(self, path.into(), include_sources).await?,
            History { page } => paginate(self.storage.history().await?, page, 100)?,
        };
        ensure!(
            serde_json::to_vec(&value)?.len() < crate::protocol::MAX_FRAME - 1024,
            "Result exceeds 1 MiB; request a single page instead of a complete collection"
        );
        let mut value = value;
        value["schema_version"] = json!(1);
        Ok(value)
    }
}
fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 256 && !name.chars().any(char::is_control),
        "Name requires 1–256 bytes without control characters"
    );
    Ok(())
}
fn network(error: anyhow::Error) -> anyhow::Error {
    RpcError::new(3, error.to_string()).into()
}
fn source(error: anyhow::Error) -> anyhow::Error {
    RpcError::new(4, error.to_string()).into()
}

fn first_page() -> u32 {
    1
}
fn paginate(items: Vec<Value>, page: u32, size: usize) -> Result<Value> {
    ensure!(
        (1..=1000).contains(&page),
        "Page must be between 1 and 1000"
    );
    let total = items.len();
    let items = items
        .into_iter()
        .skip((page as usize - 1) * size)
        .take(size)
        .collect::<Vec<_>>();
    Ok(json!({"items":items,"total":total,"page":page,"has_more":page as usize*size<total}))
}
