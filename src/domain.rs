use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    #[serde(default)]
    pub id: String,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub platform: Option<Platform>,
    #[serde(default)]
    #[serde(alias = "songID")]
    pub song_id: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub artwork: Option<String>,
    #[serde(default = "empty_object")]
    #[serde(alias = "sourceInfo")]
    pub source_info: serde_json::Value,
}

impl Track {
    pub fn local(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .context("Local audio file does not exist")?;
        ensure!(path.is_file(), "Expected a local audio file");
        let name = path
            .file_stem()
            .context("Audio file has no name")?
            .to_string_lossy()
            .into_owned();
        let path = path
            .to_str()
            .context("Audio path must be valid UTF-8")?
            .to_owned();
        let digest = Sha256::digest(path.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(Self {
            id: format!("local:{digest}"),
            title: name,
            artist: String::new(),
            path,
            platform: None,
            song_id: String::new(),
            album: String::new(),
            duration: 0.0,
            artwork: None,
            source_info: empty_object(),
        })
    }

    pub fn lx_info(&self) -> serde_json::Value {
        use serde_json::json;
        let mut info = self.source_info.clone();
        if !info.is_object() {
            info = empty_object();
        }
        let map = info.as_object_mut().expect("object initialized above");
        map.insert("name".into(), json!(self.title));
        map.insert("singer".into(), json!(self.artist));
        map.insert("source".into(), json!(self.platform));
        map.entry("songmid").or_insert(json!(self.song_id));
        map.entry("albumId").or_insert(json!(""));
        map.entry("_types").or_insert(json!({}));
        map.entry("types").or_insert(json!([]));
        map.insert("albumName".into(), json!(self.album));
        map.insert("img".into(), json!(self.artwork.as_deref().unwrap_or("")));
        map.insert(
            "interval".into(),
            json!(format!(
                "{}:{:02}",
                self.duration as u64 / 60,
                self.duration as u64 % 60
            )),
        );
        map.insert("typeUrl".into(), json!({}));
        info
    }
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Kw,
    Kg,
    Tx,
    #[default]
    Wy,
    Mg,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Kw => "kw",
            Self::Kg => "kg",
            Self::Tx => "tx",
            Self::Wy => "wy",
            Self::Mg => "mg",
        })
    }
}

impl std::str::FromStr for Platform {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "kw" => Ok(Self::Kw),
            "kg" => Ok(Self::Kg),
            "tx" => Ok(Self::Tx),
            "wy" => Ok(Self::Wy),
            "mg" => Ok(Self::Mg),
            _ => anyhow::bail!("Unknown platform: {value}"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Collection {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub artwork: Option<String>,
    pub platform: Platform,
    pub chart: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CollectionPage<T> {
    pub schema_version: u32,
    pub items: Vec<T>,
    pub total: Option<usize>,
    pub page: u32,
    pub has_more: bool,
}

impl<T> CollectionPage<T> {
    pub fn new(items: Vec<T>, total: usize, page: u32, size: usize) -> Self {
        Self {
            schema_version: 1,
            has_more: page as usize * size < total,
            items,
            total: Some(total),
            page,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalPlaylist {
    pub id: Uuid,
    pub name: String,
    pub tracks: Vec<Track>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedMedia {
    pub url: String,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueueEntry {
    pub id: Uuid,
    pub track: Track,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackState {
    #[default]
    Idle,
    Resolving,
    Buffering,
    Playing,
    Paused,
    Ended,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackMode {
    #[default]
    Sequential,
    Shuffle,
    RepeatOne,
}
impl PlaybackMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Shuffle => "shuffle",
            Self::RepeatOne => "repeat_one",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub instance_id: Uuid,
    pub sequence: u64,
    pub state: PlaybackState,
    #[serde(default)]
    pub mode: PlaybackMode,
    pub queue: Vec<QueueEntry>,
    pub current: Option<Uuid>,
    pub position: f64,
    pub duration: Option<f64>,
    pub volume: u8,
    pub error: Option<String>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            schema_version: 1,
            instance_id: Uuid::new_v4(),
            sequence: 0,
            state: PlaybackState::Idle,
            mode: PlaybackMode::Sequential,
            queue: Vec::new(),
            current: None,
            position: 0.0,
            duration: None,
            volume: 70,
            error: None,
        }
    }
}

impl Snapshot {
    pub fn current_entry(&self) -> Option<&QueueEntry> {
        self.current
            .and_then(|id| self.queue.iter().find(|entry| entry.id == id))
    }
}
